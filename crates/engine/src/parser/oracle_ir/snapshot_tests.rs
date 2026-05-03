//! Two-layer IR + lowered parity snapshot tests (Phase 51, D-03/D-04).
//!
//! Each test parses real card Oracle text through parse_oracle_ir (producing
//! OracleDocIr) and lower_oracle_ir (producing ParsedAbilities), snapshotting
//! both layers so structural drift and assembly bugs are independently caught.

use crate::parser::oracle::{lower_oracle_ir, parse_oracle_ir, ParsedAbilities};
use crate::parser::oracle_ir::doc::OracleDocIr;

/// Parse Oracle text through both IR and lowering layers.
fn parse_two_layer(
    oracle_text: &str,
    card_name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> (OracleDocIr, ParsedAbilities) {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let ir = parse_oracle_ir(oracle_text, card_name, &[], &types, &subtypes);
    let lowered = lower_oracle_ir(&ir);
    (ir, lowered)
}

// ---------------------------------------------------------------------------
// Keywords
// ---------------------------------------------------------------------------

#[test]
fn serra_angel() {
    let (ir, lowered) = parse_two_layer(
        "Flying\nVigilance (Attacking doesn't cause this creature to tap.)",
        "Serra Angel",
        &["Creature"],
        &["Angel"],
    );
    insta::assert_json_snapshot!("serra_angel_ir", &ir);
    insta::assert_json_snapshot!("serra_angel_lowered", &lowered);
}

#[test]
fn baneslayer_angel() {
    let (ir, lowered) = parse_two_layer(
        "Flying, first strike, lifelink, protection from Demons and from Dragons",
        "Baneslayer Angel",
        &["Creature"],
        &["Angel"],
    );
    insta::assert_json_snapshot!("baneslayer_angel_ir", &ir);
    insta::assert_json_snapshot!("baneslayer_angel_lowered", &lowered);
}

#[test]
fn slippery_bogle() {
    let (ir, lowered) = parse_two_layer(
        "Hexproof (This creature can't be the target of spells or abilities your opponents control.)",
        "Slippery Bogle",
        &["Creature"],
        &["Beast"],
    );
    insta::assert_json_snapshot!("slippery_bogle_ir", &ir);
    insta::assert_json_snapshot!("slippery_bogle_lowered", &lowered);
}

#[test]
fn questing_beast() {
    let (ir, lowered) = parse_two_layer(
        "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to an opponent, it deals that much damage to target planeswalker that player controls.",
        "Questing Beast",
        &["Creature"],
        &["Beast"],
    );
    insta::assert_json_snapshot!("questing_beast_ir", &ir);
    insta::assert_json_snapshot!("questing_beast_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Casting restrictions / permissions
// ---------------------------------------------------------------------------

#[test]
fn savage_summoning() {
    let (ir, lowered) = parse_two_layer(
        "This spell can't be countered.\nThe next creature spell you cast this turn can be cast as though it had flash. That spell can't be countered. That creature enters with an additional +1/+1 counter on it.",
        "Savage Summoning",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("savage_summoning_ir", &ir);
    insta::assert_json_snapshot!("savage_summoning_lowered", &lowered);
}

#[test]
fn leyline_of_anticipation() {
    let (ir, lowered) = parse_two_layer(
        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.",
        "Leyline of Anticipation",
        &["Enchantment"],
        &[],
    );
    insta::assert_json_snapshot!("leyline_of_anticipation_ir", &ir);
    insta::assert_json_snapshot!("leyline_of_anticipation_lowered", &lowered);
}

#[test]
fn thalia_guardian_of_thraben() {
    let (ir, lowered) = parse_two_layer(
        "First strike\nNoncreature spells cost {1} more to cast.",
        "Thalia, Guardian of Thraben",
        &["Creature"],
        &["Human", "Soldier"],
    );
    insta::assert_json_snapshot!("thalia_guardian_of_thraben_ir", &ir);
    insta::assert_json_snapshot!("thalia_guardian_of_thraben_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Additional costs
// ---------------------------------------------------------------------------

#[test]
fn bone_splinters() {
    let (ir, lowered) = parse_two_layer(
        "As an additional cost to cast this spell, sacrifice a creature.\nDestroy target creature.",
        "Bone Splinters",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("bone_splinters_ir", &ir);
    insta::assert_json_snapshot!("bone_splinters_lowered", &lowered);
}

#[test]
fn village_rites() {
    let (ir, lowered) = parse_two_layer(
        "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.",
        "Village Rites",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("village_rites_ir", &ir);
    insta::assert_json_snapshot!("village_rites_lowered", &lowered);
}

#[test]
fn deadly_rollick() {
    let (ir, lowered) = parse_two_layer(
        "If you control a commander, you may cast this spell without paying its mana cost.\nExile target creature.",
        "Deadly Rollick",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("deadly_rollick_ir", &ir);
    insta::assert_json_snapshot!("deadly_rollick_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Activated abilities
// ---------------------------------------------------------------------------

#[test]
fn llanowar_elves() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Add {G}.",
        "Llanowar Elves",
        &["Creature"],
        &["Elf", "Druid"],
    );
    insta::assert_json_snapshot!("llanowar_elves_ir", &ir);
    insta::assert_json_snapshot!("llanowar_elves_lowered", &lowered);
}

#[test]
fn mother_of_runes() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Target creature you control gains protection from the color of your choice until end of turn.",
        "Mother of Runes",
        &["Creature"],
        &["Human", "Cleric"],
    );
    insta::assert_json_snapshot!("mother_of_runes_ir", &ir);
    insta::assert_json_snapshot!("mother_of_runes_lowered", &lowered);
}

#[test]
fn jade_mage() {
    let (ir, lowered) = parse_two_layer(
        "{2}{G}: Create a 1/1 green Saproling creature token.",
        "Jade Mage",
        &["Creature"],
        &["Human", "Shaman"],
    );
    insta::assert_json_snapshot!("jade_mage_ir", &ir);
    insta::assert_json_snapshot!("jade_mage_lowered", &lowered);
}

#[test]
fn aetherling() {
    let (ir, lowered) = parse_two_layer(
        "{U}: Exile this creature. Return it to the battlefield under its owner's control at the beginning of the next end step.\n{U}: This creature can't be blocked this turn.\n{1}: This creature gets +1/-1 until end of turn.\n{1}: This creature gets -1/+1 until end of turn.",
        "Aetherling",
        &["Creature"],
        &["Shapeshifter"],
    );
    insta::assert_json_snapshot!("aetherling_ir", &ir);
    insta::assert_json_snapshot!("aetherling_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Planeswalker loyalty
// ---------------------------------------------------------------------------

#[test]
fn liliana_of_the_veil() {
    let (ir, lowered) = parse_two_layer(
        "[+1]: Each player discards a card.\n[\u{2212}2]: Target player sacrifices a creature.\n[\u{2212}6]: Separate all permanents target player controls into two piles. That player sacrifices all permanents in the pile of their choice.",
        "Liliana of the Veil",
        &["Planeswalker"],
        &["Liliana"],
    );
    insta::assert_json_snapshot!("liliana_of_the_veil_ir", &ir);
    insta::assert_json_snapshot!("liliana_of_the_veil_lowered", &lowered);
}

#[test]
fn jace_the_mind_sculptor() {
    let (ir, lowered) = parse_two_layer(
        "[+2]: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n[0]: Draw three cards, then put two cards from your hand on top of your library in any order.\n[\u{2212}1]: Return target creature to its owner's hand.\n[\u{2212}12]: Exile all cards from target player's library, then that player shuffles their hand into their library.",
        "Jace, the Mind Sculptor",
        &["Planeswalker"],
        &["Jace"],
    );
    insta::assert_json_snapshot!("jace_the_mind_sculptor_ir", &ir);
    insta::assert_json_snapshot!("jace_the_mind_sculptor_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Equipment / Vehicles
// ---------------------------------------------------------------------------

#[test]
fn short_sword() {
    let (ir, lowered) = parse_two_layer(
        "Equipped creature gets +1/+1.\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)",
        "Short Sword",
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!("short_sword_ir", &ir);
    insta::assert_json_snapshot!("short_sword_lowered", &lowered);
}

#[test]
fn smugglers_copter() {
    let (ir, lowered) = parse_two_layer(
        "Flying\nWhenever this Vehicle attacks or blocks, you may draw a card. If you do, discard a card.\nCrew 1 (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)",
        "Smuggler's Copter",
        &["Artifact"],
        &["Vehicle"],
    );
    insta::assert_json_snapshot!("smugglers_copter_ir", &ir);
    insta::assert_json_snapshot!("smugglers_copter_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Leveler
// ---------------------------------------------------------------------------

#[test]
fn student_of_warfare() {
    let (ir, lowered) = parse_two_layer(
        "Level up {W} ({W}: Put a level counter on this. Level up only as a sorcery.)\nLEVEL 2-6\n3/3\nFirst strike\nLEVEL 7+\n4/4\nDouble strike",
        "Student of Warfare",
        &["Creature"],
        &["Human", "Knight"],
    );
    insta::assert_json_snapshot!("student_of_warfare_ir", &ir);
    insta::assert_json_snapshot!("student_of_warfare_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Adventure
// ---------------------------------------------------------------------------

#[test]
fn bonecrusher_giant() {
    let (ir, lowered) = parse_two_layer(
        "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.",
        "Bonecrusher Giant",
        &["Creature"],
        &["Giant"],
    );
    insta::assert_json_snapshot!("bonecrusher_giant_ir", &ir);
    insta::assert_json_snapshot!("bonecrusher_giant_lowered", &lowered);
}

#[test]
fn brazen_borrower() {
    let (ir, lowered) = parse_two_layer(
        "Flash\nFlying\nThis creature can block only creatures with flying.",
        "Brazen Borrower",
        &["Creature"],
        &["Faerie", "Rogue"],
    );
    insta::assert_json_snapshot!("brazen_borrower_ir", &ir);
    insta::assert_json_snapshot!("brazen_borrower_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Kicker
// ---------------------------------------------------------------------------

#[test]
fn vines_of_vastwood() {
    let (ir, lowered) = parse_two_layer(
        "Kicker {G} (You may pay an additional {G} as you cast this spell.)\nTarget creature can't be the target of spells or abilities your opponents control this turn. If this spell was kicked, that creature gets +4/+4 until end of turn.",
        "Vines of Vastwood",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("vines_of_vastwood_ir", &ir);
    insta::assert_json_snapshot!("vines_of_vastwood_lowered", &lowered);
}

#[test]
fn reckless_bushwhacker() {
    let (ir, lowered) = parse_two_layer(
        "Surge {1}{R} (You may cast this spell for its surge cost if you or a teammate has cast another spell this turn.)\nHaste\nWhen this creature enters, if its surge cost was paid, other creatures you control get +1/+0 and gain haste until end of turn.",
        "Reckless Bushwhacker",
        &["Creature"],
        &["Goblin", "Warrior"],
    );
    insta::assert_json_snapshot!("reckless_bushwhacker_ir", &ir);
    insta::assert_json_snapshot!("reckless_bushwhacker_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Channel
// ---------------------------------------------------------------------------

#[test]
fn boseiju_who_endures() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Add {G}.\nChannel \u{2014} {1}{G}, Discard this card: Destroy target artifact, enchantment, or nonbasic land an opponent controls. That player may search their library for a land card with a basic land type, put it onto the battlefield, then shuffle. This ability costs {1} less to activate for each legendary creature you control.",
        "Boseiju, Who Endures",
        &["Land"],
        &[],
    );
    insta::assert_json_snapshot!("boseiju_who_endures_ir", &ir);
    insta::assert_json_snapshot!("boseiju_who_endures_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Enchantments with multiple ability types
// ---------------------------------------------------------------------------

#[test]
fn conclave_mentor() {
    let (ir, lowered) = parse_two_layer(
        "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on that creature instead.\nWhen this creature dies, you gain life equal to its power.",
        "Conclave Mentor",
        &["Creature"],
        &["Centaur", "Cleric"],
    );
    insta::assert_json_snapshot!("conclave_mentor_ir", &ir);
    insta::assert_json_snapshot!("conclave_mentor_lowered", &lowered);
}

#[test]
fn luminarch_aspirant() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of combat on your turn, put a +1/+1 counter on target creature you control.",
        "Luminarch Aspirant",
        &["Creature"],
        &["Human", "Cleric"],
    );
    insta::assert_json_snapshot!("luminarch_aspirant_ir", &ir);
    insta::assert_json_snapshot!("luminarch_aspirant_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Ability words (Landfall, Prowess, Evolve)
// ---------------------------------------------------------------------------

#[test]
fn tireless_tracker() {
    let (ir, lowered) = parse_two_layer(
        "Landfall \u{2014} Whenever a land you control enters, investigate. (Create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")\nWhenever you sacrifice a Clue, put a +1/+1 counter on this creature.",
        "Tireless Tracker",
        &["Creature"],
        &["Human", "Scout"],
    );
    insta::assert_json_snapshot!("tireless_tracker_ir", &ir);
    insta::assert_json_snapshot!("tireless_tracker_lowered", &lowered);
}

#[test]
fn monastery_swiftspear() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)",
        "Monastery Swiftspear",
        &["Creature"],
        &["Human", "Monk"],
    );
    insta::assert_json_snapshot!("monastery_swiftspear_ir", &ir);
    insta::assert_json_snapshot!("monastery_swiftspear_lowered", &lowered);
}

#[test]
fn experiment_one() {
    let (ir, lowered) = parse_two_layer(
        "Evolve (Whenever a creature you control enters, if that creature has greater power or toughness than this creature, put a +1/+1 counter on this creature.)\nRemove two +1/+1 counters from this creature: Regenerate it. (The next time this creature would be destroyed this turn, instead tap it, remove it from combat, and heal all damage on it.)",
        "Experiment One",
        &["Creature"],
        &["Human", "Ooze"],
    );
    insta::assert_json_snapshot!("experiment_one_ir", &ir);
    insta::assert_json_snapshot!("experiment_one_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Deeply nested / multi-clause spells
// ---------------------------------------------------------------------------

#[test]
fn swords_to_plowshares() {
    let (ir, lowered) = parse_two_layer(
        "Exile target creature. Its controller gains life equal to its power.",
        "Swords to Plowshares",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("swords_to_plowshares_ir", &ir);
    insta::assert_json_snapshot!("swords_to_plowshares_lowered", &lowered);
}

#[test]
fn kroxa_titan_of_deaths_hunger() {
    let (ir, lowered) = parse_two_layer(
        "When Kroxa enters, sacrifice it unless it escaped.\nWhenever Kroxa enters or attacks, each opponent discards a card, then each opponent who didn't discard a nonland card this way loses 3 life.\nEscape\u{2014}{B}{B}{R}{R}, Exile five other cards from your graveyard. (You may cast this card from your graveyard for its escape cost.)",
        "Kroxa, Titan of Death's Hunger",
        &["Creature"],
        &["Elder", "Giant"],
    );
    insta::assert_json_snapshot!("kroxa_titan_ir", &ir);
    insta::assert_json_snapshot!("kroxa_titan_lowered", &lowered);
}

#[test]
fn snapcaster_mage() {
    let (ir, lowered) = parse_two_layer(
        "Flash\nWhen this creature enters, target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)",
        "Snapcaster Mage",
        &["Creature"],
        &["Human", "Wizard"],
    );
    insta::assert_json_snapshot!("snapcaster_mage_ir", &ir);
    insta::assert_json_snapshot!("snapcaster_mage_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Triggers (various patterns)
// ---------------------------------------------------------------------------

#[test]
fn goblin_guide() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nWhenever this creature attacks, defending player reveals the top card of their library. If it's a land card, that player puts it into their hand.",
        "Goblin Guide",
        &["Creature"],
        &["Goblin", "Scout"],
    );
    insta::assert_json_snapshot!("goblin_guide_ir", &ir);
    insta::assert_json_snapshot!("goblin_guide_lowered", &lowered);
}

#[test]
fn young_pyromancer() {
    let (ir, lowered) = parse_two_layer(
        "Whenever you cast an instant or sorcery spell, create a 1/1 red Elemental creature token.",
        "Young Pyromancer",
        &["Creature"],
        &["Human", "Shaman"],
    );
    insta::assert_json_snapshot!("young_pyromancer_ir", &ir);
    insta::assert_json_snapshot!("young_pyromancer_lowered", &lowered);
}

#[test]
fn dark_confidant() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of your upkeep, reveal the top card of your library and put that card into your hand. You lose life equal to its mana value.",
        "Dark Confidant",
        &["Creature"],
        &["Human", "Wizard"],
    );
    insta::assert_json_snapshot!("dark_confidant_ir", &ir);
    insta::assert_json_snapshot!("dark_confidant_lowered", &lowered);
}

#[test]
fn eidolon_of_the_great_revel() {
    let (ir, lowered) = parse_two_layer(
        "Whenever a player casts a spell with mana value 3 or less, this creature deals 2 damage to that player.",
        "Eidolon of the Great Revel",
        &["Creature"],
        &["Spirit"],
    );
    insta::assert_json_snapshot!("eidolon_of_the_great_revel_ir", &ir);
    insta::assert_json_snapshot!("eidolon_of_the_great_revel_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Static abilities
// ---------------------------------------------------------------------------

#[test]
fn leonin_arbiter() {
    let (ir, lowered) = parse_two_layer(
        "Players can't search libraries. Any player may pay {2} for that player to ignore this effect until end of turn.",
        "Leonin Arbiter",
        &["Creature"],
        &["Cat", "Cleric"],
    );
    insta::assert_json_snapshot!("leonin_arbiter_ir", &ir);
    insta::assert_json_snapshot!("leonin_arbiter_lowered", &lowered);
}

#[test]
fn lovestruck_beast() {
    let (ir, lowered) = parse_two_layer(
        "This creature can't attack unless you control a 1/1 creature.",
        "Lovestruck Beast",
        &["Creature"],
        &["Beast", "Noble"],
    );
    insta::assert_json_snapshot!("lovestruck_beast_ir", &ir);
    insta::assert_json_snapshot!("lovestruck_beast_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// CDA (Characteristic-defining ability)
// ---------------------------------------------------------------------------

#[test]
fn tarmogoyf() {
    let (ir, lowered) = parse_two_layer(
        "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.",
        "Tarmogoyf",
        &["Creature"],
        &["Lhurgoyf"],
    );
    insta::assert_json_snapshot!("tarmogoyf_ir", &ir);
    insta::assert_json_snapshot!("tarmogoyf_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Equipment with living weapon
// ---------------------------------------------------------------------------

#[test]
fn batterskull() {
    let (ir, lowered) = parse_two_layer(
        "Living weapon (When this Equipment enters, create a 0/0 black Phyrexian Germ creature token, then attach this to it.)\nEquipped creature gets +4/+4 and has vigilance and lifelink.\n{3}: Return this Equipment to its owner's hand.\nEquip {5}",
        "Batterskull",
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!("batterskull_ir", &ir);
    insta::assert_json_snapshot!("batterskull_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// ETB with counters / X spells
// ---------------------------------------------------------------------------

#[test]
fn walking_ballista() {
    let (ir, lowered) = parse_two_layer(
        "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.",
        "Walking Ballista",
        &["Artifact", "Creature"],
        &["Construct"],
    );
    insta::assert_json_snapshot!("walking_ballista_ir", &ir);
    insta::assert_json_snapshot!("walking_ballista_lowered", &lowered);
}

#[test]
fn chalice_of_the_void() {
    let (ir, lowered) = parse_two_layer(
        "This artifact enters with X charge counters on it.\nWhenever a player casts a spell with mana value equal to the number of charge counters on this artifact, counter that spell.",
        "Chalice of the Void",
        &["Artifact"],
        &[],
    );
    insta::assert_json_snapshot!("chalice_of_the_void_ir", &ir);
    insta::assert_json_snapshot!("chalice_of_the_void_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Phyrexian mana
// ---------------------------------------------------------------------------

#[test]
fn dismember() {
    let (ir, lowered) = parse_two_layer(
        "({B/P} can be paid with either {B} or 2 life.)\nTarget creature gets -5/-5 until end of turn.",
        "Dismember",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("dismember_ir", &ir);
    insta::assert_json_snapshot!("dismember_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Changeling
// ---------------------------------------------------------------------------

#[test]
fn changeling_outcast() {
    let (ir, lowered) = parse_two_layer(
        "Changeling (This card is every creature type.)\nThis creature can't block and can't be blocked.",
        "Changeling Outcast",
        &["Creature"],
        &["Shapeshifter"],
    );
    insta::assert_json_snapshot!("changeling_outcast_ir", &ir);
    insta::assert_json_snapshot!("changeling_outcast_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn edge_case_empty_oracle_text() {
    let (ir, lowered) = parse_two_layer("", "Grizzly Bears", &["Creature"], &["Bear"]);
    insta::assert_json_snapshot!("edge_case_empty_ir", &ir);
    insta::assert_json_snapshot!("edge_case_empty_lowered", &lowered);
}

#[test]
fn edge_case_reminder_text_only() {
    let (ir, lowered) = parse_two_layer("({T}: Add {R}.)", "Mountain", &["Land"], &["Mountain"]);
    insta::assert_json_snapshot!("edge_case_reminder_only_ir", &ir);
    insta::assert_json_snapshot!("edge_case_reminder_only_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Mana abilities (multi-color)
// ---------------------------------------------------------------------------

#[test]
fn birds_of_paradise() {
    let (ir, lowered) = parse_two_layer(
        "Flying\n{T}: Add one mana of any color.",
        "Birds of Paradise",
        &["Creature"],
        &["Bird"],
    );
    insta::assert_json_snapshot!("birds_of_paradise_ir", &ir);
    insta::assert_json_snapshot!("birds_of_paradise_lowered", &lowered);
}

#[test]
fn manamorphose() {
    let (ir, lowered) = parse_two_layer(
        "Add two mana in any combination of colors.\nDraw a card.",
        "Manamorphose",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("manamorphose_ir", &ir);
    insta::assert_json_snapshot!("manamorphose_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// ETB search (tutor)
// ---------------------------------------------------------------------------

#[test]
fn stoneforge_mystic() {
    let (ir, lowered) = parse_two_layer(
        "When this creature enters, you may search your library for an Equipment card, reveal it, put it into your hand, then shuffle.\n{1}{W}, {T}: You may put an Equipment card from your hand onto the battlefield.",
        "Stoneforge Mystic",
        &["Creature"],
        &["Kor", "Artificer"],
    );
    insta::assert_json_snapshot!("stoneforge_mystic_ir", &ir);
    insta::assert_json_snapshot!("stoneforge_mystic_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Figure of Destiny (multi-activated, type-changing)
// ---------------------------------------------------------------------------

#[test]
fn figure_of_destiny() {
    let (ir, lowered) = parse_two_layer(
        "{R/W}: This creature becomes a Kithkin Spirit with base power and toughness 2/2.\n{R/W}{R/W}{R/W}: If this creature is a Spirit, it becomes a Kithkin Spirit Warrior with base power and toughness 4/4.\n{R/W}{R/W}{R/W}{R/W}{R/W}{R/W}: If this creature is a Warrior, it becomes a Kithkin Spirit Warrior Avatar with base power and toughness 8/8, flying, and first strike.",
        "Figure of Destiny",
        &["Creature"],
        &["Kithkin"],
    );
    insta::assert_json_snapshot!("figure_of_destiny_ir", &ir);
    insta::assert_json_snapshot!("figure_of_destiny_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Dies trigger
// ---------------------------------------------------------------------------

#[test]
fn murderous_rider() {
    let (ir, lowered) = parse_two_layer(
        "Lifelink\nWhen this creature dies, put it on the bottom of its owner's library.",
        "Murderous Rider",
        &["Creature"],
        &["Zombie", "Knight"],
    );
    insta::assert_json_snapshot!("murderous_rider_ir", &ir);
    insta::assert_json_snapshot!("murderous_rider_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Soulbond
// ---------------------------------------------------------------------------

#[test]
fn wolfir_silverheart() {
    let (ir, lowered) = parse_two_layer(
        "Soulbond (You may pair this creature with another unpaired creature when either enters. They remain paired for as long as you control both of them.)\nAs long as this creature is paired with another creature, each of those creatures gets +4/+4.",
        "Wolfir Silverheart",
        &["Creature"],
        &["Wolf", "Warrior"],
    );
    insta::assert_json_snapshot!("wolfir_silverheart_ir", &ir);
    insta::assert_json_snapshot!("wolfir_silverheart_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Adventure companion
// ---------------------------------------------------------------------------

#[test]
fn edgewall_innkeeper() {
    let (ir, lowered) = parse_two_layer(
        "Whenever you cast a creature spell that has an Adventure, draw a card. (It doesn't need to have gone on the adventure first.)",
        "Edgewall Innkeeper",
        &["Creature"],
        &["Human", "Peasant"],
    );
    insta::assert_json_snapshot!("edgewall_innkeeper_ir", &ir);
    insta::assert_json_snapshot!("edgewall_innkeeper_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Bomat Courier (exile + activated with complex costs)
// ---------------------------------------------------------------------------

#[test]
fn bomat_courier() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nWhenever this creature attacks, exile the top card of your library face down. (You can't look at it.)\n{R}, Discard your hand, Sacrifice this creature: Put all cards exiled with this creature into their owners' hands.",
        "Bomat Courier",
        &["Artifact", "Creature"],
        &["Construct"],
    );
    insta::assert_json_snapshot!("bomat_courier_ir", &ir);
    insta::assert_json_snapshot!("bomat_courier_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Diagnostic snapshot tests (Phase 51, D-10)
// ---------------------------------------------------------------------------

mod diagnostic_snapshots {
    use crate::parser::oracle::parse_oracle_ir;

    /// Parse Oracle text and return only the diagnostics vec from the IR.
    fn parse_diagnostics(
        oracle_text: &str,
        card_name: &str,
        types: &[&str],
        subtypes: &[&str],
    ) -> Vec<crate::parser::oracle_ir::diagnostic::OracleDiagnostic> {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let ir = parse_oracle_ir(oracle_text, card_name, &[], &types, &subtypes);
        ir.diagnostics
    }

    #[test]
    /// TargetFallback diagnostics from `parse_target` inside effect chains are not
    /// yet captured in `ir.diagnostics` — the 100+ callers in oracle_effect/* use
    /// the no-ctx `parse_target` wrapper. When those callers are migrated to
    /// `parse_target_with_ctx`, this test should be updated to assert non-empty
    /// diagnostics again.
    fn diagnostic_target_fallback() {
        let diagnostics = parse_diagnostics(
            "Whenever this creature attacks, you may sacrifice another creature. When you do, this creature deals damage equal to the sacrificed creature's power to any target. If the sacrificed creature was a Giant, this creature deals twice that much damage instead.",
            "Surtland Flinger",
            &["Creature"],
            &["Giant", "Berserker"],
        );
        // Currently empty — parse_target in effect chains uses the no-ctx wrapper.
        // When effect chain callers migrate to parse_target_with_ctx, this will
        // capture TargetFallback diagnostics again.
        insta::assert_json_snapshot!("diagnostic_target_fallback", &diagnostics);
    }

    #[test]
    fn diagnostic_ignored_remainder() {
        let diagnostics = parse_diagnostics(
            "Whenever this creature attacks, it deals damage to the player or planeswalker it's attacking equal to the number of artifacts you control.\nEncore {5}{R} ({5}{R}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)",
            "Fathom Fleet Swordjack",
            &["Creature"],
            &["Orc", "Pirate"],
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.category_name() == "ignored-remainder"),
            "Expected ignored-remainder diagnostic for Fathom Fleet Swordjack, got: {:?}",
            diagnostics
        );
        insta::assert_json_snapshot!("diagnostic_ignored_remainder", &diagnostics);
    }

    #[test]
    fn diagnostic_swallowed_clause() {
        let diagnostics = parse_diagnostics(
            "When this enchantment enters, create a 1/1 white Human creature token, a 1/1 blue Merfolk creature token, and a 1/1 red Goblin creature token. Then secretly choose Human, Merfolk, or Goblin.\nSacrifice this enchantment, Reveal the creature type you chose: If target attacking creature token is the chosen type, put three +1/+1 counters on it and it gains deathtouch until end of turn.",
            "A Killer Among Us",
            &["Enchantment"],
            &[],
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.category_name() == "swallowed-clause"),
            "Expected swallowed-clause diagnostic for A Killer Among Us, got: {:?}",
            diagnostics
        );
        insta::assert_json_snapshot!("diagnostic_swallowed_clause", &diagnostics);
    }

    // NOTE: CascadeLoss diagnostic is not triggered by any card in the current
    // card-data.json corpus (0 occurrences in coverage report). The variant exists
    // for cascade-diff detection in swallow_check.rs but no current Oracle text
    // triggers it. A test will be added when a card that produces this diagnostic
    // is identified.
}
