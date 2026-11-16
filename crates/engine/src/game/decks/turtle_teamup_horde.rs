//! Teenage Mutant Ninja Turtles "Turtle Team-Up" — the concrete decklists for
//! the `ChallengeDeck::TurtleTeamUpHorde` Boss seat.
//!
//! Data, not logic. The Boss side fields TWO piles from the product's 38-card
//! enemy deck (verified against Scryfall set `ttmc`, "Teenage Mutant Ninja
//! Turtles Eternal Tokens"):
//!
//! * the **event deck** (27 cards: 10 Ninja creatures + 17 Events), loaded as
//!   the Boss seat's library and played through the Action-phase wave —
//!   creatures enter the battlefield, Events resolve one-shot and go to the
//!   Boss graveyard;
//! * the **Boss deck** (11 Bosses), loaded as the face-down Boss pile and
//!   deployed in three stages (1 → 2 → 3) sharing one life total per stage.
//!
//! Every enemy-deck card is a token-layout card on Scryfall, so NONE of them
//! resolves in the card DB — all 38 are predefined catalog tokens, selected by
//! preset id (the `known-tokens.overlay.toml` TTMC rows). Id-pinning also
//! carries each token's preset IDENTITY onto its library object, so the
//! reveal/deploy path can graft the card's code-synthesized abilities (Boss
//! triggers, Event one-shots, Ninja can't-block/max-blockers statics — the
//! catalog materializer only parses static lines and cannot express them).
//!
//! Product-vocabulary readings used by the synthesizer (`game::boss`):
//! "the bosses" = the Boss seat, "the heroes" = every survivor seat, "each
//! hero" = each survivor, "the boss graveyard/library" = the Boss seat's
//! zones, "number of bosses" = deployed (face-up, command-zone) Bosses.
//!
//! Deferred (explicit gaps, pinned by `event_resolution_status_is_pinned` in
//! the integration suite): the eight Events that need hero choices — Baxter's
//! Experiments (copy the next event), Chrome Dome's Overload, Krang's
//! Stratagem, Leatherhead's Smackdown, Rat King's Revolution, Savanti
//! Romero's Curse, Villain Infiltration, and Villain Mayhem! — resolve to no
//! effect until the team-choice UI, AI legal actions, and the supporting
//! quantities (boss count, stun counters) land. Smackdown and Infiltration
//! share the greatest-power-choice shape, so one implementation unlocks both.

/// No card-DB cards: every Turtle Team-Up enemy card is token-layout on
/// Scryfall (`ttmc` is a token set) and lives in the token catalog instead.
/// Empty by construction — the integration deck-names guard skips resolution
/// for an empty list, and `event_deck_is_exactly_the_published_27` below pins
/// the real pile.
pub const TURTLE_TEAMUP_NONTOKEN_CARDS: &[(u32, &str)] = &[];

/// The 27-card event deck as `(count, preset-id)` pairs: 10 Ninja creatures
/// (5 Foot Disciple, 3 Foot Skirmisher, 2 Foot Enforcer) + 17 Events (one of
/// each named Event, 2 Villain Infiltration, 3 Villain Mayhem!, 1 Villain
/// Scheme!). Loaded as the Boss seat's library.
pub const TURTLE_TEAMUP_EVENT_DECK_TOKENS: &[(u32, &str)] = &[
    // Ninja creatures (10).
    (5, "2e194383-a7e5-535c-bd30-33de7d583270"), // Foot Disciple 3/3
    (3, "ad49d3fe-7432-5d67-9d5a-9d3acf8c37bf"), // Foot Skirmisher 2/1 flying
    (2, "d21023d3-b5e4-5f00-b712-25eee35d6f26"), // Foot Enforcer 6/3 green
    // Named Events, one of each (11).
    (1, "c27bf2f4-73a2-52b6-80e9-1016e1b53927"), // Event: Baxter's Experiments
    (1, "8fa1fc05-4093-5f4c-9fa1-2e7570f0640d"), // Event: Bebop's Rampage
    (1, "4005a147-ab7c-592c-bfc1-0fbdb03de48b"), // Event: Chrome Dome's Overload
    (1, "a4b473e6-557b-51f2-a105-dc149ce2c492"), // Event: Karai's Planning
    (1, "c9b0f651-8762-51d7-a951-3627fc5ee6e3"), // Event: Krang's Stratagem
    (1, "f13c9091-91b6-51b4-91d5-214180ff03ee"), // Event: Leatherhead's Smackdown
    (1, "e579a333-1d17-50cb-a375-2f73d0c9ca53"), // Event: Rat King's Revolution
    (1, "d175c6cf-f7d1-5ccc-ab94-c64294e63ae9"), // Event: Rocksteady's Beatdown
    (1, "bb6fd47b-b2ad-5307-911c-4d9abe26c250"), // Event: Savanti Romero's Curse
    (1, "afed8326-d4bf-5ffa-8ad0-892aecc47340"), // Event: Shredder's Challenge
    (1, "8d293c7b-471b-51a4-a26a-19e4c9179f2c"), // Event: Slash's Smash
    // Generic Events (6).
    (2, "f2227981-b536-5664-ba74-9d90303c3f33"), // Event: Villain Infiltration
    (3, "2688b47f-bd1d-575f-a976-41add511ea95"), // Event: Villain Mayhem!
    (1, "62b81bb8-4da6-512c-ac02-c9146fdd01df"), // Event: Villain Scheme!
];

/// The 11-card Boss deck as `(count, preset-id)` pairs, one of each Boss.
/// Loaded as the face-down Boss pile; stages deploy 1 → 2 → 3 per game.
pub const TURTLE_TEAMUP_BOSS_DECK_TOKENS: &[(u32, &str)] = &[
    (1, "1d0641fa-c4fb-5683-a8ba-1ffa6f692aea"), // Baxter, Exzzperimental Zzientist
    (1, "58baca03-3374-5a17-b9cc-70039a25b289"), // Bebop, Porcine Punk
    (1, "aa8dcb67-bffa-5bb5-8ec4-6cfa6e7f87c0"), // Chrome Dome, Cyber-Ninja
    (1, "727e8ec1-1cc9-573c-b20d-3b7bb1308ef8"), // Karai, Shadow Warrior
    (1, "df03a49b-89dd-536c-8d5a-819cb0318e4b"), // Krang, Dimension X Overlord
    (1, "4b53113d-c11a-552a-8310-bb545967f1ab"), // Leatherhead, Sewer Gator
    (1, "7a707dae-da05-5137-a759-2136ac39ad55"), // Rat King, the Chaosbringer
    (1, "717f93b4-f34d-5b89-9266-b693ce89af91"), // Rocksteady, Rowdy Rhino
    (1, "6c2eccfb-af84-57ff-bf35-ec008da2d458"), // Savanti Romero, Temporal Rogue
    (1, "f164b86f-196f-5372-8c7e-279b7d1281de"), // Shredder, Foot Clan Overlord
    (1, "8a66162e-10cd-5f12-ad48-dcad19b41de2"), // Slash, the Dark Mirror
];

/// Total event-deck card count (for coverage assertions / library-size sanity).
pub fn event_deck_card_count() -> u32 {
    TURTLE_TEAMUP_EVENT_DECK_TOKENS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total Boss-deck card count.
pub fn boss_deck_card_count() -> u32 {
    TURTLE_TEAMUP_BOSS_DECK_TOKENS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_deck_is_exactly_the_published_27() {
        // 10 Ninjas (5/3/2) + 17 Events (11 named + 2 Infiltration + 3 Mayhem + 1 Scheme).
        assert_eq!(event_deck_card_count(), 27);
        let ninjas: u32 = TURTLE_TEAMUP_EVENT_DECK_TOKENS
            .iter()
            .take(3)
            .map(|(count, _)| *count)
            .sum();
        assert_eq!(ninjas, 10);
        let events: u32 = TURTLE_TEAMUP_EVENT_DECK_TOKENS
            .iter()
            .skip(3)
            .map(|(count, _)| *count)
            .sum();
        assert_eq!(events, 17);
    }

    #[test]
    fn boss_deck_is_exactly_the_published_11() {
        assert_eq!(boss_deck_card_count(), 11);
        assert_eq!(TURTLE_TEAMUP_BOSS_DECK_TOKENS.len(), 11);
    }

    #[test]
    fn enemy_deck_is_exactly_the_published_38() {
        assert_eq!(event_deck_card_count() + boss_deck_card_count(), 38);
    }

    /// Every selector must resolve to its preset by id (the path
    /// `deck_loading::load_horde_library` takes first), so a mistyped or
    /// retired id fails here rather than panicking at game start.
    #[test]
    fn token_selectors_resolve_to_the_intended_presets() {
        for (_, selector) in TURTLE_TEAMUP_EVENT_DECK_TOKENS
            .iter()
            .chain(TURTLE_TEAMUP_BOSS_DECK_TOKENS.iter())
        {
            let preset = crate::game::token_presets::known_token_preset_by_id(selector)
                .unwrap_or_else(|| {
                    panic!("Turtle Team-Up selector '{selector}' must resolve to a preset")
                });
            assert_eq!(&preset.id, selector);
        }
    }

    /// Pin each preset's body shape: Bosses are Boss-typed, Events are
    /// Event-typed, Ninjas are Ninja creatures with the published P/T. A
    /// catalog swap to a same-name body with different characteristics would
    /// change how the deck plays, so it fails loudly here.
    #[test]
    fn preset_bodies_carry_the_published_characteristics() {
        use crate::types::card_type::CoreType;
        for (_, selector) in TURTLE_TEAMUP_BOSS_DECK_TOKENS {
            let preset = crate::game::token_presets::known_token_preset_by_id(selector)
                .expect("boss selector resolves");
            assert_eq!(preset.body.core_types, vec![CoreType::Boss]);
            assert!(preset.body.subtypes.is_empty());
        }
        for (index, (_, selector)) in TURTLE_TEAMUP_EVENT_DECK_TOKENS.iter().enumerate() {
            let preset = crate::game::token_presets::known_token_preset_by_id(selector)
                .expect("event-deck selector resolves");
            if index < 3 {
                // Ninjas, in list order: Disciple 3/3, Skirmisher 2/1, Enforcer 6/3.
                let (power, toughness) = [(3, 3), (2, 1), (6, 3)][index];
                assert_eq!(preset.body.core_types, vec![CoreType::Creature]);
                assert_eq!(preset.body.subtypes, vec!["Ninja".to_string()]);
                assert_eq!(preset.body.power, Some(power));
                assert_eq!(preset.body.toughness, Some(toughness));
            } else {
                assert_eq!(preset.body.core_types, vec![CoreType::Event]);
            }
        }
        // Skirmisher flies (keyword on the body); the Enforcer is green.
        let skirmisher = crate::game::token_presets::known_token_preset_by_id(
            TURTLE_TEAMUP_EVENT_DECK_TOKENS[1].1,
        )
        .expect("skirmisher resolves");
        assert!(skirmisher
            .body
            .keywords
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Flying)));
        let enforcer = crate::game::token_presets::known_token_preset_by_id(
            TURTLE_TEAMUP_EVENT_DECK_TOKENS[2].1,
        )
        .expect("enforcer resolves");
        assert_eq!(
            enforcer.body.colors,
            vec![crate::types::mana::ManaColor::Green]
        );
    }
}
