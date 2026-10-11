//! "<color> or <type>" and "X or Y" phrases read as a union of both kinds, driven
//! through `apply()`: Soldevi Adnate's sacrifice choices, the damage-source
//! replacements of Mechanized Warfare and Pyromancer's Swath, and the Champion
//! payloads of Lightning Crafter and Unstoppable Ash. Members are built from
//! their Oracle text; every other card is real.
use engine::ai_support::legal_actions_full;
use engine::game::apply;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const ADNATE: &str = "{T}, Sacrifice a black or artifact creature: Add an amount of {B} equal to the sacrificed creature's mana value.";
const WARFARE: &str = "If a red or artifact source you control would deal damage to an opponent or a permanent an opponent controls, it deals that much damage plus 1 instead.";
const SWATH: &str = "If an instant or sorcery source you control would deal damage to a permanent or player, it deals that much damage plus 2 to that permanent or player instead.\nAt the beginning of each end step, discard your hand.";
const CRAFTER: &str = "Champion a Goblin or Shaman (When this enters, sacrifice it unless you exile another Goblin or Shaman you control. When this leaves the battlefield, that card returns to the battlefield.)\n{T}: This creature deals 3 damage to any target.";
const ASH: &str = "Trample\nChampion a Treefolk or Warrior (When this enters, sacrifice it unless you exile another Treefolk or Warrior you control. When this leaves the battlefield, that card returns to the battlefield.)\nWhenever a creature you control becomes blocked, it gets +0/+5 until end of turn.";
const CLIQUE: &str = "Flash\nFlying\nChampion a Faerie (When this enters, sacrifice it unless you exile another Faerie you control. When this leaves the battlefield, that card returns to the battlefield.)\nWhen a Faerie is championed with this creature, tap all lands target player controls.";

const ADNATE_FODDER: [&str; 4] = [
    "Walking Corpse",
    "Myr Retriever",
    "Grizzly Bears",
    "Illuminor Szeras",
];

/// Soldevi Adnate's ability activated by hand: the creatures its sacrifice
/// offers, and the pool after sacrificing Walking Corpse when it is offered.
fn adnate_offers(from_oracle: bool) -> (Vec<&'static str>, Vec<String>) {
    let db = shared_card_db().expect("card db");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let adnate = if from_oracle {
        let mut b = s.add_creature_from_oracle(P0, "Soldevi Adnate", 1, 2, ADNATE);
        b.with_subtypes(vec!["Human", "Cleric"]);
        b.id()
    } else {
        s.add_real_card(P0, "Soldevi Adnate", Zone::Battlefield, db)
    };
    let ids: Vec<ObjectId> = ADNATE_FODDER
        .iter()
        .map(|name| s.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    let mut r = s.build();
    engine::game::layers::flush_layers(r.state_mut());
    apply(
        r.state_mut(),
        P0,
        GameAction::ActivateAbility {
            source_id: adnate,
            ability_index: 0,
        },
    )
    .expect("Soldevi Adnate activates");
    let WaitingFor::PayCost { choices, .. } = &r.state().waiting_for else {
        panic!(
            "expected a sacrifice choice, got {:?}",
            r.state().waiting_for
        );
    };
    let offered: Vec<&str> = ADNATE_FODDER
        .iter()
        .zip(&ids)
        .filter(|(_, id)| choices.contains(id))
        .map(|(name, _)| *name)
        .collect();
    let mut pool = Vec::new();
    if offered.contains(&"Walking Corpse") {
        apply(
            r.state_mut(),
            P0,
            GameAction::SelectCards {
                cards: vec![ids[0]],
            },
        )
        .expect("sacrificing Walking Corpse pays the cost");
        pool = r.state().players[0]
            .mana_pool
            .units()
            .map(|unit| format!("{:?}", unit.color))
            .collect();
    }
    (offered, pool)
}

/// CR 105.2 + CR 205.2a + CR 106.4: "a black or artifact creature" offers the
/// black Walking Corpse and the artifact Myr Retriever, never Grizzly Bears, and
/// sacrificing Corpse adds {B}{B} (its mana value).
#[test]
fn adnate_sacrifice_offers_both_kinds() {
    if shared_card_db().is_none() {
        return;
    }
    let (offered, pool) = adnate_offers(true);
    // Reach: the activation's choice is live at all.
    assert!(offered.contains(&"Illuminor Szeras"), "{offered:?}");
    assert_eq!(
        offered,
        vec!["Walking Corpse", "Myr Retriever", "Illuminor Szeras"]
    );
    assert_eq!(pool, vec!["Black", "Black"]);
}

/// The card loader reads the stored parse, so the loaded Adnate offers both
/// kinds once the card data carries the union.
#[test]
fn loaded_adnate_sacrifice_offers_both_kinds() {
    if shared_card_db().is_none() {
        return;
    }
    let (offered, pool) = adnate_offers(false);
    assert!(offered.contains(&"Illuminor Szeras"), "{offered:?}");
    assert_eq!(
        offered,
        vec!["Walking Corpse", "Myr Retriever", "Illuminor Szeras"]
    );
    assert_eq!(pool, vec!["Black", "Black"]);
}

/// Who deals the damage and to whom.
struct DamageLeg {
    source: &'static str,
    zone: Zone,
    lands: usize,
    controller: PlayerId,
}

/// (label, replacement card and its Oracle text, damage leg, life lost by (P0, P1)).
type DamageRow = (
    &'static str,
    (&'static str, &'static str),
    DamageLeg,
    (i32, i32),
);

/// Life lost by (P0, P1) when `leg.source` deals its damage to the other
/// player with `replacement` (an enchantment built from its Oracle text) on
/// P0's battlefield; an opponent's leg runs in that opponent's main phase.
fn damage_dealt(
    replacement: Option<(&str, &str)>,
    extra: Option<&str>,
    leg: &DamageLeg,
) -> (i32, i32) {
    let db = shared_card_db().expect("card db");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    if let Some((name, text)) = replacement {
        s.add_enchantment_from_oracle(P0, name, text);
    }
    if let Some(name) = extra {
        s.add_real_card(P0, name, Zone::Battlefield, db);
    }
    let source = s.add_real_card(leg.controller, leg.source, leg.zone, db);
    for _ in 0..leg.lands {
        s.add_real_card(leg.controller, "Mountain", Zone::Battlefield, db);
    }
    let mut r = s.build();
    engine::game::layers::flush_layers(r.state_mut());
    let life = |r: &engine::game::scenario::GameRunner| {
        (r.state().players[0].life, r.state().players[1].life)
    };
    let before = life(&r);
    if leg.controller != P0 {
        let st = r.state_mut();
        st.active_player = leg.controller;
        st.priority_player = leg.controller;
        st.waiting_for = WaitingFor::Priority {
            player: leg.controller,
        };
    }
    let aim = if leg.controller == P0 { P1 } else { P0 };
    if leg.zone == Zone::Hand {
        let _ = r.cast(source).target_player(aim).resolve();
    } else {
        let index = r.state().objects[&source]
            .abilities
            .iter()
            .position(|a| a.kind == AbilityKind::Activated)
            .expect("an activated damage ability");
        let _ = r.activate(source, index).target_player(aim).resolve();
    }
    let after = life(&r);
    (before.0 - after.0, before.1 - after.1)
}

/// CR 609.7 + CR 614.1a: each replacement adds to damage from a source of
/// either kind it names, and to no other source or controller.
#[test]
fn damage_source_union_replacements() {
    if shared_card_db().is_none() {
        return;
    }
    let leg = |source, zone, lands, controller| DamageLeg {
        source,
        zone,
        lands,
        controller,
    };
    let (h, b) = (Zone::Hand, Zone::Battlefield);
    // Reach: a one-kind replacement (Fire Servant) doubles Shock on this driver.
    assert_eq!(
        damage_dealt(None, Some("Fire Servant"), &leg("Shock", h, 1, P0)),
        (0, 4)
    );
    let warfare = ("Mechanized Warfare", WARFARE);
    let swath = ("Pyromancer's Swath", SWATH);
    let rows: [DamageRow; 10] = [
        (
            "warfare: artifact Rod",
            warfare,
            leg("Rod of Ruin", b, 3, P0),
            (0, 2),
        ),
        (
            "warfare: red Shock",
            warfare,
            leg("Shock", h, 1, P0),
            (0, 3),
        ),
        (
            "warfare: neither",
            warfare,
            leg("Prodigal Sorcerer", b, 0, P0),
            (0, 1),
        ),
        (
            "warfare: opponent's Rod",
            warfare,
            leg("Rod of Ruin", b, 3, P1),
            (1, 0),
        ),
        (
            "warfare: opponent's Shock",
            warfare,
            leg("Shock", h, 1, P1),
            (2, 0),
        ),
        (
            "swath: sorcery Lava Axe",
            swath,
            leg("Lava Axe", h, 5, P0),
            (0, 7),
        ),
        (
            "swath: instant Shock",
            swath,
            leg("Shock", h, 1, P0),
            (0, 4),
        ),
        (
            "swath: neither",
            swath,
            leg("Rod of Ruin", b, 3, P0),
            (0, 1),
        ),
        (
            "swath: opponent's Lava Axe",
            swath,
            leg("Lava Axe", h, 5, P1),
            (5, 0),
        ),
        (
            "swath: opponent's Shock",
            swath,
            leg("Shock", h, 1, P1),
            (2, 0),
        ),
    ];
    for (tag, replacement, leg, expected) in rows {
        assert_eq!(
            damage_dealt(Some(replacement), None, &leg),
            expected,
            "{tag}"
        );
    }
}

/// A Champion creature built from its Oracle text, cast with its lands.
struct Championer {
    name: &'static str,
    text: &'static str,
    subtypes: &'static [&'static str],
    pt: (i32, i32),
    cost: &'static [ManaCostShard],
    generic: u32,
    land: &'static str,
    lands: usize,
}

/// (label, member, other permanents, expected (member zone, other zones)).
type ChampionRow<'a> = (
    &'a str,
    &'a Championer,
    &'a [(PlayerId, &'a str)],
    (Zone, Vec<Zone>),
);

/// Cast `member` with `others` on the battlefield and answer its enters
/// trigger by exiling one of `others` when an action names it. Returns the
/// member's zone and each other card's zone.
fn champion_end(member: &Championer, others: &[(PlayerId, &str)]) -> (Zone, Vec<Zone>) {
    let db = shared_card_db().expect("card db");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let id = {
        let mut b = s.add_creature_to_hand_from_oracle(
            P0,
            member.name,
            member.pt.0,
            member.pt.1,
            member.text,
        );
        b.with_subtypes(member.subtypes.to_vec());
        b.with_mana_cost(ManaCost::Cost {
            shards: member.cost.to_vec(),
            generic: member.generic,
        });
        b.id()
    };
    let other_ids: Vec<ObjectId> = others
        .iter()
        .map(|(owner, name)| s.add_real_card(*owner, name, Zone::Battlefield, db))
        .collect();
    for _ in 0..member.lands {
        s.add_real_card(P0, member.land, Zone::Battlefield, db);
    }
    let mut r = s.build();
    engine::game::layers::flush_layers(r.state_mut());
    let card_id = r.state().objects[&id].card_id;
    apply(
        r.state_mut(),
        P0,
        GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("the championing creature is castable");
    for _ in 0..16 {
        match &r.state().waiting_for {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => break,
            WaitingFor::Priority { player } => {
                let player = *player;
                apply(r.state_mut(), player, GameAction::PassPriority).expect("pass");
            }
            _ => {
                let (flat, _, _) = legal_actions_full(r.state());
                let pick = other_ids
                    .iter()
                    .find_map(|o| flat.iter().find(|a| a.related_object_ids().contains(o)))
                    .or_else(|| flat.first())
                    .cloned()
                    .expect("an answer to the champion prompt");
                let actor = r.state().waiting_for.acting_player().unwrap_or(P0);
                apply(r.state_mut(), actor, pick).expect("the answer applies");
            }
        }
    }
    let zone = |id: &ObjectId| r.state().objects[id].zone;
    (zone(&id), other_ids.iter().map(zone).collect())
}

const CRAFTER_MEMBER: Championer = Championer {
    name: "Lightning Crafter",
    text: CRAFTER,
    subtypes: &["Goblin", "Shaman"],
    pt: (3, 3),
    cost: &[ManaCostShard::Red],
    generic: 3,
    land: "Mountain",
    lands: 4,
};

const ASH_MEMBER: Championer = Championer {
    name: "Unstoppable Ash",
    text: ASH,
    subtypes: &["Treefolk", "Warrior"],
    pt: (5, 5),
    cost: &[ManaCostShard::Green],
    generic: 3,
    land: "Forest",
    lands: 4,
};

const CLIQUE_MEMBER: Championer = Championer {
    name: "Mistbind Clique",
    text: CLIQUE,
    subtypes: &["Faerie", "Wizard"],
    pt: (4, 4),
    cost: &[ManaCostShard::Blue],
    generic: 3,
    land: "Island",
    lands: 4,
};

/// CR 702.72a: "Champion a Goblin or Shaman" exiles another Goblin or another
/// Shaman you control; anything else, or nothing, sacrifices the championer.
#[test]
fn champion_union_payloads() {
    if shared_card_db().is_none() {
        return;
    }
    use Zone::{Battlefield as Bf, Exile as Ex, Graveyard as Gy};
    // Reach: a one-kind Champion exiles on this driver.
    assert_eq!(
        champion_end(&CLIQUE_MEMBER, &[(P0, "Pestermite")]),
        (Bf, vec![Ex])
    );
    let rows: [ChampionRow<'_>; 8] = [
        (
            "crafter + Shaman",
            &CRAFTER_MEMBER,
            &[(P0, "Jade Mage")],
            (Bf, vec![Ex]),
        ),
        (
            "crafter + Goblin",
            &CRAFTER_MEMBER,
            &[(P0, "Goblin Piker")],
            (Bf, vec![Ex]),
        ),
        (
            "crafter + neither",
            &CRAFTER_MEMBER,
            &[(P0, "Grizzly Bears")],
            (Gy, vec![Bf]),
        ),
        (
            "crafter + opponent's Shaman",
            &CRAFTER_MEMBER,
            &[(P1, "Jade Mage")],
            (Gy, vec![Bf]),
        ),
        ("crafter alone", &CRAFTER_MEMBER, &[], (Gy, vec![])),
        (
            "ash + Warrior",
            &ASH_MEMBER,
            &[(P0, "Goblin Piker")],
            (Bf, vec![Ex]),
        ),
        (
            "ash + Treefolk",
            &ASH_MEMBER,
            &[(P0, "Ironroot Treefolk")],
            (Bf, vec![Ex]),
        ),
        (
            "ash + neither",
            &ASH_MEMBER,
            &[(P0, "Grizzly Bears")],
            (Gy, vec![Bf]),
        ),
    ];
    for (tag, member, others, expected) in rows {
        assert_eq!(champion_end(member, others), expected, "{tag}");
    }
}

#[allow(clippy::too_many_arguments)]
const fn championer(
    name: &'static str,
    text: &'static str,
    subtypes: &'static [&'static str],
    pt: (i32, i32),
    cost: &'static [ManaCostShard],
    generic: u32,
    land: &'static str,
    lands: usize,
) -> Championer {
    Championer {
        name,
        text,
        subtypes,
        pt,
        cost,
        generic,
        land,
        lands,
    }
}

const MOB: &str = "Champion a Goblin (When this enters, sacrifice it unless you exile another Goblin you control. When this leaves the battlefield, that card returns to the battlefield.)\nWhenever a Goblin you control deals combat damage to a player, you may create a 1/1 black Goblin Rogue creature token.";
const NOVA: &str = "Trample\nChampion an Elemental (When this enters, sacrifice it unless you exile another Elemental you control. When this leaves the battlefield, that card returns to the battlefield.)";
const EXEMPLAR: &str = "Flying\nChampion an Elemental (When this enters, sacrifice it unless you exile another Elemental you control. When this leaves the battlefield, that card returns to the battlefield.)";
const TRIO: &str = "First strike, vigilance\nChampion a Kithkin (When this enters, sacrifice it unless you exile another Kithkin you control. When this leaves the battlefield, that card returns to the battlefield.)\nThis creature can block any number of creatures.";
const PROPHETS: &str = "Champion a Merfolk (When this enters, sacrifice it unless you exile another Merfolk you control. When this leaves the battlefield, that card returns to the battlefield.)\nWhenever this creature deals combat damage to a player, you may sacrifice a Merfolk. If you do, take an extra turn after this one.";
const PACKMASTER: &str = "Champion an Elf (When this creature enters, sacrifice it unless you exile another Elf you control. When this creature leaves the battlefield, that card returns to the battlefield.)\n{2}{G}: Create a 2/2 green Wolf creature token.\nWolves you control have deathtouch.";
const HERO: &str = "Changeling (This card is every creature type.)\nChampion a creature (When this enters, sacrifice it unless you exile another creature you control. When this leaves the battlefield, that card returns to the battlefield.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)";

/// CR 702.72a + CR 109.2 + CR 205.3m: a Champion payload naming a creature type
/// exiles another permanent of that type you control, kindred enchantments and
/// artifacts included; "Champion a creature" still needs a creature.
#[test]
fn champion_kindred_permanents() {
    if shared_card_db().is_none() {
        return;
    }
    use ManaCostShard::{Black, Blue, Green, Red, White};
    use Zone::{Battlefield as Bf, Exile as Ex, Graveyard as Gy};
    let hero = championer(
        "Changeling Hero",
        HERO,
        &["Shapeshifter"],
        (4, 4),
        &[White],
        4,
        "Plains",
        5,
    );
    // Reach: "Champion a creature" exiles a creature on this driver.
    assert_eq!(
        champion_end(&hero, &[(P0, "Grizzly Bears")]),
        (Bf, vec![Ex])
    );
    let mob = championer(
        "Boggart Mob",
        MOB,
        &["Goblin", "Warrior"],
        (5, 5),
        &[Black],
        3,
        "Swamp",
        4,
    );
    let nova = championer(
        "Nova Chaser",
        NOVA,
        &["Elemental", "Warrior"],
        (10, 2),
        &[Red],
        3,
        "Mountain",
        4,
    );
    let exemplar = championer(
        "Supreme Exemplar",
        EXEMPLAR,
        &["Elemental"],
        (10, 10),
        &[Blue],
        6,
        "Island",
        7,
    );
    let trio = championer(
        "Thoughtweft Trio",
        TRIO,
        &["Kithkin", "Soldier"],
        (5, 5),
        &[White, White],
        2,
        "Plains",
        4,
    );
    let prophets = championer(
        "Wanderwine Prophets",
        PROPHETS,
        &["Merfolk", "Wizard"],
        (4, 4),
        &[Blue, Blue],
        4,
        "Island",
        6,
    );
    let packmaster = championer(
        "Wren's Run Packmaster",
        PACKMASTER,
        &["Elf", "Warrior"],
        (5, 5),
        &[Green],
        3,
        "Forest",
        4,
    );
    let kindred = (Bf, vec![Ex]);
    let rows: [ChampionRow<'_>; 12] = [
        (
            "clique + Bitterblossom",
            &CLIQUE_MEMBER,
            &[(P0, "Bitterblossom")],
            kindred.clone(),
        ),
        (
            "crafter + Boggart Shenanigans",
            &CRAFTER_MEMBER,
            &[(P0, "Boggart Shenanigans")],
            kindred.clone(),
        ),
        (
            "mob + Boggart Shenanigans",
            &mob,
            &[(P0, "Boggart Shenanigans")],
            kindred.clone(),
        ),
        (
            "crafter + Thornbite Staff",
            &CRAFTER_MEMBER,
            &[(P0, "Thornbite Staff")],
            kindred.clone(),
        ),
        (
            "nova + Eyes of the Wisent",
            &nova,
            &[(P0, "Eyes of the Wisent")],
            kindred.clone(),
        ),
        (
            "exemplar + Eyes of the Wisent",
            &exemplar,
            &[(P0, "Eyes of the Wisent")],
            kindred.clone(),
        ),
        (
            "trio + Militia's Pride",
            &trio,
            &[(P0, "Militia's Pride")],
            kindred.clone(),
        ),
        (
            "prophets + Merrow Commerce",
            &prophets,
            &[(P0, "Merrow Commerce")],
            kindred.clone(),
        ),
        (
            "packmaster + Prowess of the Fair",
            &packmaster,
            &[(P0, "Prowess of the Fair")],
            kindred.clone(),
        ),
        (
            "ash + Obsidian Battle-Axe",
            &ASH_MEMBER,
            &[(P0, "Obsidian Battle-Axe")],
            kindred.clone(),
        ),
        (
            "clique + opponent's Bitterblossom",
            &CLIQUE_MEMBER,
            &[(P1, "Bitterblossom")],
            (Gy, vec![Bf]),
        ),
        (
            "hero + Bitterblossom",
            &hero,
            &[(P0, "Bitterblossom")],
            (Gy, vec![Bf]),
        ),
    ];
    for (tag, member, others, expected) in rows {
        assert_eq!(champion_end(member, others), expected, "{tag}");
    }
}
