//! Format context for an LLM opponent: which Magic format is being played, and
//! how a player approaches it.
//!
//! The game engine already knows the format — it is `GameState::format_config`,
//! fixed when the game is created — and a draft pod knows its `DraftKind`. A
//! model that is not told is left to guess whether it is in a two-player
//! 60-card duel, a four-player Commander pod, or a 40-card Limited game, and
//! those want different play: who should be the aggressor, whether to hold
//! interaction, whether being the biggest threat is a liability.
//!
//! # Two kinds of statement
//!
//! - **Facts** come from the engine's [`engine::types::format::FormatConfig`] and [`GameState`] (permitted
//!   and current player counts, starting life, deck-size rule, singleton, commander damage). They are read, never
//!   restated, so a format whose rules change cannot drift from the prompt.
//! - **Strategy** is the per-format approach from the format strategy guide:
//!   the mindset a player brings and the tendencies that follow from it. It is
//!   deliberately general. Named decks are not carried over — metagames move,
//!   and a model told which deck is "top" will play as if that were still true.
//!
//! Every statement here is engine-authored and static, so it belongs in the
//! system prompt, outside the untrusted-data fence: none of it is rendered from
//! card text, player names, or provider output.
//!
//! # Freeform
//!
//! `Freeform`, `FreeformCommander`, `Dandan`, and `Custom` receive [`GENERIC_STRATEGY`]
//! for opponents whose decks are unknown, plus their configured facts. A custom format may
//! still impose specific deck and card-pool rules.

use engine::types::format::{FormatTopology, GameFormat};
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use phase_ai::config::AiDifficulty;

/// Principles that hold in every format. Appended after the format's own
/// strategy so a format that says nothing about, say, mulligans still inherits
/// them.
pub const UNIVERSAL_PRINCIPLES: &str = "In every format: decide whether you are the beatdown \
     (the player who should pressure) or the control in this matchup and play to that role; \
     track your resources, your opponents' open mana and graveyards, and what they could be \
     holding; know your outs and theirs; and when deciding whether to mulligan, remember that \
     a functional hand with fewer cards beats a non-functional seven.";

const DIFFICULTY_PRECEDENCE: &str = "If this format guidance and your playing-strength \
     description disagree about how deeply to reason, how far ahead to plan, how fast or how \
     aggressively to play, follow your playing-strength description.";

/// Advice for playing against unknown opponents. Custom formats may still fix
/// their card pools and deck rules; the engine-derived facts state those rules.
pub const GENERIC_STRATEGY: &str =
    "Do not assume anything about your opponents' decks beyond what you can see. \
     Play a consistent, proactive game: develop your mana and board, apply pressure with \
     what you have shown you can protect, and keep interaction for the cards that actually \
     threaten you.";

// ── Strategy text, one entry per format family ───────────────────────────────
//
// Each is the mindset a player brings, not a decklist. Wording follows the
// format strategy guide; where a format's pool is a moving target the guide says
// to verify the live metagame, so these name tendencies rather than decks.

const STANDARD: &str = "Standard: a small, rotating card pool, so decks are streamlined and \
     reward synergy and a coherent plan. Expect fast aggro, midrange value, and control. \
     Proactive, consistent plans are favoured; work out early who is the beatdown, and plan for \
     the mirror and for the two or three most common archetypes.";

const LIMITED: &str = "Limited (draft or sealed): a deck built from a pool, with about 16-17 \
     lands, so power is lower and games are decided by card quality, curve, and removal. Bombs, \
     evasive threats, and efficient removal are the strongest cards. Develop on curve, make \
     efficient trades, and count the race before attacking. Aggressive, consistent decks tend \
     to beat clunky ones.";

const PIONEER: &str = "Pioneer: a streamlined 'Standard plus' format where aggro, combo, and \
     midrange are closely matched and mana bases are good but not perfect. Fair decks live or \
     die by whether they interact early, so deal with the pieces of an opposing combo or aggro \
     plan before they come online.";

const MODERN: &str = "Modern: a fast format in which each deck is tuned to do something powerful, \
     often by turn four. Be proactive; a reactive plan needs very efficient answers. Respect \
     graveyard strategies, artifacts and enchantments, and combo, and weigh speed against life \
     loss where fetch lands, shock lands, and pain lands are in play.";

const PREMODERN: &str = "Premodern: a fundamentals-first format of removal, counterspells, \
     creature quality, and card advantage, with longer games than Modern. Play to the board and \
     make efficient trades; overextending into a sweeper such as Wrath of God is a real risk. \
     Blue control and tempo are strong because counterspells are cheap.";

const LEGACY: &str = "Legacy: powerful decks kept honest by cheap interaction — cheap and free \
     counterspells, discard, and removal. Tempo and disruption are strong: cheap threats backed \
     by interaction punish slow draws. Combo punishes durdling, so keep relevant answers or \
     pressure available, and sequence card-selection spells such as Brainstorm carefully to hide \
     information.";

const VINTAGE: &str =
    "Vintage: raw power is the baseline, so speed, redundancy, and the timing of \
     interaction decide games, and many games end on turns one to three. Assume an opponent can \
     win on the spot; hold cheap interaction (free spells, counterspells, hate pieces) and use \
     it at the right moment. Tutors and card selection find the right card for the matchup; \
     favour hands that can both execute and interact.";

const PAUPER: &str = "Pauper: commons only, so no single bomb decides games — card quality, mana, \
     and tempo do. Card advantage and value engines beat decks that only trade one-for-one. \
     Aggro needs efficient creatures and reach such as burn; control needs a real plan to close \
     the game, since commons rarely win quickly.";

const HISTORIC: &str = "Historic: a non-rotating Arena format with powerful digital-only cards. \
     The best decks are streamlined versions of established archetypes. Play to the board, \
     expect well-tuned archetypes, and read digital-only card text carefully.";

const TIMELESS: &str = "Timeless: an extremely high-powered Arena format. Speed and interaction \
     matter. Play to the board and be ready for explosive turns.";

// CR 903.8: a commander may be cast from the command zone for an additional {2}
// per previous cast, so recasting gets steadily more expensive.
// CR 903.10a: 21 or more combat damage from the same commander eliminates a player.
const COMMANDER: &str = "Commander: manage resources and plan for a long game. Assess which \
     opponent is the real threat, and consider when to hold removal rather than spend it. Your commander is a \
     repeatable engine: protect it, and remember each recast costs more in commander tax. \
     Commander damage is tracked per commander, so watch who is accumulating it.";

const COMMANDER_MULTIPLAYER: &str = "With more than two players, politics matter: avoid being \
     the first or biggest threat; develop your resources while other opponents fight.";

const COMMANDER_TWO_PLAYER: &str = "In a two-player game, play the head-to-head matchup directly.";

const COMMANDER_SINGLETON: &str = "Singleton means redundancy comes from different cards with \
     similar effects.";

const COMMANDER_DRAFT: &str = "Commander Draft: your deck came from a draft pool rather than a \
     tuned list, so play the strengths of the cards you actually have rather than an \
     idealised plan.";

const PAUPER_COMMANDER: &str = "Pauper Commander: a lower-power, social Commander variant where \
     common-based synergy and value decks dominate. Look for repeatable value engines and good \
     removal, and play around the synergy your commander gives your deck.";

const DUEL_COMMANDER: &str = "Duel Commander: a two-player, tempo-oriented Commander variant that \
     plays closer to Legacy than to multiplayer EDH. Be proactive: tempo and commander damage \
     matter more, and board wipes are less useful. Your commander is a repeatable threat you can \
     recast, so plan around commander tax.";

const TINY_LEADERS: &str = "Tiny Leaders: a low-curve singleton format where mana value 3 or less \
     shapes everything. Tempo and efficiency matter: curve out with efficient threats and \
     acceleration rather than waiting for expensive bombs.";

const OATHBREAKER: &str = "Oathbreaker: a tight, strategic singleton format built around a \
     planeswalker and a signature spell in the command zone. Use the planeswalker's repeatable \
     ability and the signature spell as a reliable engine. Games usually run faster than \
     Commander because decks are smaller.";

const BRAWL: &str = "Brawl: a commander format with a restricted card pool, so games tend to \
     feel more focused than in unrestricted Commander. Build your play around your commander \
     and your deck's synergy, within the deck size and card pool the format facts above state.";

const FREE_FOR_ALL: &str = "Free-for-all: every player is playing for themselves. Manage \
     resources and assess each opponent's position.";

const FREE_FOR_ALL_MULTIPLAYER: &str = "With more than two players, politics can matter: avoid \
     becoming the biggest threat while other opponents fight.";

// CR 810.9: damage, life loss, and life gain happen to each player individually
// and the result is applied to the team's shared life total.
// CR 810.8a: players win and lose the game only as a team.
const TWO_HEADED_GIANT: &str = "Two-Headed Giant: you and your teammate share one life total and \
     win or lose together. Damage and life gain apply to the team's total, so coordinate with \
     your partner, protect the shared life total, and pressure the opposing team's.";

// CR 904.1: one archenemy, strengthened by scheme cards, faces a team.
const ARCHENEMY: &str = "Archenemy: one archenemy strengthened by scheme cards faces a team of \
     heroes. The heroes should coordinate against the archenemy and its schemes; the archenemy \
     should leverage its schemes to snowball.";

// CR 901.1: Planechase adds plane and phenomenon cards to a normal game.
const PLANECHASE: &str = "Planechase: plane cards and the planar die add chaos to a normal game. \
     Do not over-rely on a fixed board state, and account for the current plane's effect when \
     planning your turn.";

const MOMIR: &str = "Momir's Madness: the deck is only basic lands and the game is driven by the \
     Momir emblem. Almost everything is luck, but mana efficiency matters: choose when to make \
     a large creature and when a small one.";

/// The format-specific strategy for a built-in format, or `None` for a format
/// that has no fixed approach and takes [`GENERIC_STRATEGY`].
///
/// Exhaustive over [`GameFormat`] with no wildcard: a new built-in format must
/// decide here whether it teaches something specific, and the compiler holds
/// that decision.
fn format_strategy(format: GameFormat) -> Option<&'static [&'static str]> {
    let parts: &'static [&'static str] = match format {
        GameFormat::Standard => &[STANDARD],
        GameFormat::Limited => &[LIMITED],
        GameFormat::Pioneer => &[PIONEER],
        GameFormat::Modern => &[MODERN],
        GameFormat::Premodern => &[PREMODERN],
        GameFormat::Legacy => &[LEGACY],
        GameFormat::Vintage => &[VINTAGE],
        GameFormat::Pauper => &[PAUPER],
        GameFormat::Historic => &[HISTORIC],
        GameFormat::Timeless => &[TIMELESS],
        GameFormat::Commander => &[COMMANDER],
        GameFormat::CommanderDraft => &[COMMANDER_DRAFT, COMMANDER],
        GameFormat::PauperCommander => &[PAUPER_COMMANDER],
        GameFormat::DuelCommander => &[DUEL_COMMANDER],
        GameFormat::TinyLeaders => &[TINY_LEADERS],
        GameFormat::Oathbreaker => &[OATHBREAKER],
        GameFormat::Brawl | GameFormat::HistoricBrawl => &[BRAWL],
        GameFormat::FreeForAll => &[FREE_FOR_ALL],
        GameFormat::TwoHeadedGiant => &[TWO_HEADED_GIANT],
        GameFormat::Archenemy => &[ARCHENEMY],
        GameFormat::Planechase => &[PLANECHASE],
        GameFormat::Momir => &[MOMIR],
        // No format-specific approach to teach; configured facts still apply.
        GameFormat::Freeform
        | GameFormat::FreeformCommander
        | GameFormat::Dandan
        | GameFormat::Custom(_) => {
            return None;
        }
    };
    Some(parts)
}

/// The engine-derived facts of a game format, as one sentence.
///
/// Read the resolved config and current table from `state` so a custom format
/// reports the rules it actually runs under and the seat count actually playing.
fn format_facts(state: &GameState, viewer: PlayerId) -> String {
    let config = &state.format_config;
    let players = if config.min_players == config.max_players {
        format!("exactly {} players", config.max_players)
    } else {
        format!(
            "allows {}-{} players",
            config.min_players, config.max_players
        )
    };
    // CR 103.4 / CR 810.4 / CR 904.5: Starting life is individual except
    // for a shared team total; the archenemy and heroes have different totals.
    let life = config.starting_life_total_for_player(viewer);
    let starting_life = match config.topology() {
        FormatTopology::IndividualSeats => format!("{life} individual starting life"),
        FormatTopology::FixedTeams { .. } => format!("{life} shared team starting life"),
        FormatTopology::OneVsMany { archenemy, .. } if viewer == archenemy => {
            format!("you are the archenemy with {life} individual starting life")
        }
        FormatTopology::OneVsMany { .. } => {
            format!("you are a hero with {life} individual starting life")
        }
    };
    let mut facts = vec![
        players,
        format!("currently {} players", state.players.len()),
        starting_life,
        format!("a deck of {} cards", config.deck_size.requirement_phrase()),
    ];
    if config.singleton {
        facts.push("singleton".to_string());
    }
    if config.team_based {
        facts.push("team-based".to_string());
    }
    if let Some(threshold) = config.commander_damage_threshold {
        // CR 903.10a: this much combat damage from one commander eliminates a player.
        facts.push(format!("{threshold} commander damage eliminates a player"));
    }
    format!("{}: {}.", config.format.label(), facts.join(", "))
}

/// The format section of an in-game system prompt.
///
/// The lowest difficulty receives the facts only. Difficulty is the lever that
/// makes an LLM seat comparable to a heuristic seat at the same setting, and a
/// brand-new player does not know format theory — knowing the player count and
/// starting life is not the same as knowing how to play to a metagame.
///
/// The section yields to the difficulty brief on reasoning depth and pace:
/// format strategy says what the game is about, not how well to play it.
pub fn game_format_brief(state: &GameState, viewer: PlayerId, difficulty: AiDifficulty) -> String {
    let config = &state.format_config;
    let facts = format_facts(state, viewer);
    if matches!(difficulty, AiDifficulty::VeryEasy) {
        return format!("FORMAT: {facts}");
    }
    let mut strategy = match format_strategy(config.format) {
        Some(parts) => parts.join("\n"),
        None => GENERIC_STRATEGY.to_string(),
    };
    if matches!(
        config.format,
        GameFormat::Commander | GameFormat::CommanderDraft
    ) {
        strategy.push('\n');
        strategy.push_str(if state.players.len() > 2 {
            COMMANDER_MULTIPLAYER
        } else {
            COMMANDER_TWO_PLAYER
        });
        // CR 903.5b / CR 903.13f: Commander is singleton, while Commander
        // Draft permits repeated names from the drafted card pool.
        if config.singleton {
            strategy.push('\n');
            strategy.push_str(COMMANDER_SINGLETON);
        }
    } else if matches!(config.format, GameFormat::FreeForAll) && state.players.len() > 2 {
        strategy.push('\n');
        strategy.push_str(FREE_FOR_ALL_MULTIPLAYER);
    }
    format!("FORMAT: {facts}\n{strategy}\n{UNIVERSAL_PRINCIPLES}\n{DIFFICULTY_PRECEDENCE}")
}

// ── Draft strategy ───────────────────────────────────────────────────────────

/// Draft-pod guidance. Gated with the rest of the draft rendering so a game-only
/// consumer (`engine-wasm`) neither links `draft-core` nor carries this text.
#[cfg(feature = "draft")]
mod draft {
    use super::*;
    use draft_core::types::DraftKind;
    use draft_core::view::DraftSourceView;

    const BOOSTER_DRAFT: &str =
        "Booster draft: stay flexible early, settle on two colours (rarely \
         three) around picks 6-8 of the first pack, and prioritise removal, bombs, and a smooth \
         curve. Read signals — which colours are still flowing tells you what the players passing \
         to you are not taking. In pack 1 take the best card; in pack 2 adjust to what flowed; in \
         pack 3 fill the gaps in your curve.";

    const CUBE_DRAFT: &str =
        "This is a cube: its power level and themes decide how to draft. Every \
         pick is usually strong, so synergy and a coherent archetype matter more than raw card \
         power. Draft mana fixing and a plan, since there is no filler.";

    const COMMANDER_DRAFT_PICKS: &str = "Commander draft: you draft for a multiplayer Commander \
         game, so value cards that fit the plan of a multiplayer game — ramp, card advantage, \
         removal, and a commander you will want to cast repeatedly.";

    /// The format section of a draft system prompt.
    ///
    /// What is being drafted — the procedure, and whether the card source is a
    /// cube — is already stated as data in the user message; this adds how a drafter
    /// approaches it.
    pub fn draft_format_brief(
        view: &draft_core::view::DraftPlayerView,
        difficulty: AiDifficulty,
    ) -> String {
        if matches!(difficulty, AiDifficulty::VeryEasy) {
            return String::new();
        }
        // This brief belongs to a current-pack pick. Sealed deckbuilding and
        // Winston shared-stack decisions do not enter this builder.
        let pick_guidance = match view.kind {
            DraftKind::Quick | DraftKind::Premier | DraftKind::Traditional => BOOSTER_DRAFT,
            DraftKind::CommanderDraft => COMMANDER_DRAFT_PICKS,
            DraftKind::Sealed | DraftKind::Winston => return String::new(),
        };
        let mut parts = vec![pick_guidance];
        if matches!(view.source, DraftSourceView::Cube { .. }) {
            parts.push(CUBE_DRAFT);
        }
        parts.push(
            "Limited decks are built around card quality, a smooth curve, and removal; \
             aggressive, consistent decks tend to beat clunky good-stuff piles.",
        );
        format!(
            "FORMAT GUIDANCE:\n{}\n{DIFFICULTY_PRECEDENCE}",
            parts.join("\n")
        )
    }
}

#[cfg(feature = "draft")]
pub use draft::draft_format_brief;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::{UNTRUSTED_DATA_BEGIN, UNTRUSTED_DATA_END};
    use engine::types::custom_format::old_school_93_94;
    use engine::types::format::FormatConfig;

    const DIFFICULTIES: [AiDifficulty; 6] = [
        AiDifficulty::VeryEasy,
        AiDifficulty::Easy,
        AiDifficulty::Medium,
        AiDifficulty::Hard,
        AiDifficulty::VeryHard,
        AiDifficulty::CEDH,
    ];

    /// The registry is the engine's own list of every user-selectable built-in
    /// format, so iterating it covers a format added after this module was written.
    fn builtin_configs() -> Vec<FormatConfig> {
        GameFormat::registry()
            .into_iter()
            .map(|meta| meta.default_config)
            .collect()
    }

    fn brief(config: FormatConfig, difficulty: AiDifficulty) -> String {
        let players = config.min_players;
        let state = GameState::new(config, players, 1);
        game_format_brief(&state, PlayerId(0), difficulty)
    }

    #[test]
    fn every_builtin_format_gets_a_brief_naming_that_format() {
        for config in builtin_configs() {
            let brief = brief(config.clone(), AiDifficulty::Medium);
            assert!(
                brief.contains(&*config.format.label()),
                "{} brief does not name its format: {brief}",
                config.format
            );
            assert!(brief.contains(UNIVERSAL_PRINCIPLES), "{brief}");
        }
    }

    /// Built-in Freeform formats take generic guidance; other built-ins teach
    /// an approach of their own.
    #[test]
    fn only_freeform_formats_take_the_generic_strategy() {
        for config in builtin_configs() {
            let brief = brief(config.clone(), AiDifficulty::Medium);
            let generic = brief.contains(GENERIC_STRATEGY);
            let expects_generic = matches!(
                config.format,
                GameFormat::Freeform | GameFormat::FreeformCommander | GameFormat::Dandan
            );
            assert_eq!(
                generic, expects_generic,
                "{} generic={generic}: {brief}",
                config.format
            );
        }
    }

    #[test]
    fn a_custom_format_takes_generic_guidance_and_its_own_facts() {
        let config = FormatConfig::for_custom_rules(&old_school_93_94().rules);
        let brief = brief(config, AiDifficulty::Hard);
        assert!(brief.contains(GENERIC_STRATEGY), "{brief}");
        assert!(brief.contains("at least 60"), "{brief}");
        assert!(!brief.contains("no fixed card pool"), "{brief}");
        assert!(
            !brief.contains("no fixed card pool, power level, or deck rule"),
            "{brief}"
        );
    }

    #[test]
    fn distinct_formats_do_not_share_a_strategy_unless_the_guide_merges_them() {
        // Brawl and Historic Brawl are one entry in the guide.
        let shared = [(GameFormat::Brawl, GameFormat::HistoricBrawl)];
        for config in builtin_configs() {
            for other in builtin_configs() {
                if config.format == other.format {
                    continue;
                }
                let a = format_strategy(config.format);
                let b = format_strategy(other.format);
                if a.is_none() || b.is_none() || a != b {
                    continue;
                }
                assert!(
                    shared.contains(&(config.format, other.format))
                        || shared.contains(&(other.format, config.format)),
                    "{} and {} share a strategy",
                    config.format,
                    other.format
                );
            }
        }
    }

    #[test]
    fn facts_come_from_the_engine_config() {
        let commander = brief(FormatConfig::commander(), AiDifficulty::Medium);
        assert!(
            commander.contains("40 individual starting life"),
            "{commander}"
        );
        assert!(commander.contains("singleton"), "{commander}");
        assert!(
            commander.contains("21 commander damage eliminates a player"),
            "{commander}"
        );
        assert!(commander.contains("exactly 100"), "{commander}");

        let modern = brief(FormatConfig::modern(), AiDifficulty::Medium);
        assert!(modern.contains("exactly 2 players"), "{modern}");
        assert!(modern.contains("currently 2 players"), "{modern}");
        assert!(modern.contains("20 individual starting life"), "{modern}");
        assert!(!modern.contains("singleton"), "{modern}");
    }

    /// A custom format may fix its card pool and deck rules (the Old School
    /// preset does), so the generic text must not deny rules that the facts beside
    /// it state.
    #[test]
    fn generic_guidance_does_not_deny_the_rules_the_facts_state() {
        assert!(!GENERIC_STRATEGY.contains("no fixed"), "{GENERIC_STRATEGY}");
        assert!(
            !GENERIC_STRATEGY.contains("deck rule"),
            "{GENERIC_STRATEGY}"
        );
    }

    /// Commander Draft is not singleton (CR 903.13f(2)) and Historic Brawl is 100
    /// cards, so neither may inherit unconditional singleton or 60-card claims.
    #[test]
    fn shared_commander_and_brawl_text_does_not_contradict_the_variants() {
        let commander_draft = brief(FormatConfig::commander_draft(), AiDifficulty::Hard);
        assert!(commander_draft.contains(COMMANDER_DRAFT));
        assert!(
            !commander_draft.contains("Singleton means"),
            "{commander_draft}"
        );

        let historic = brief(FormatConfig::historic_brawl(), AiDifficulty::Hard);
        assert!(!historic.contains("60-card"), "{historic}");
        assert!(historic.contains("exactly 100"), "{historic}");
    }

    /// Commander permits two players, so multiplayer politics is conditional.
    #[test]
    fn commander_politics_is_conditional_on_more_than_two_players() {
        let two = brief(FormatConfig::commander(), AiDifficulty::Hard);
        assert!(two.contains("allows 2-6 players"), "{two}");
        assert!(two.contains(COMMANDER_TWO_PLAYER), "{two}");
        assert!(!two.contains(COMMANDER_MULTIPLAYER), "{two}");

        let config = FormatConfig::commander();
        let state = GameState::new(config, 4, 1);
        let four = game_format_brief(&state, PlayerId(0), AiDifficulty::Hard);
        assert!(four.contains(COMMANDER_MULTIPLAYER), "{four}");
        assert!(!four.contains(COMMANDER_TWO_PLAYER), "{four}");
    }

    #[test]
    fn the_lowest_difficulty_gets_facts_but_no_strategy() {
        let very_easy_brief = brief(FormatConfig::modern(), AiDifficulty::VeryEasy);
        assert!(very_easy_brief.contains("Modern"), "{very_easy_brief}");
        assert!(!very_easy_brief.contains(MODERN), "{very_easy_brief}");
        assert!(
            !very_easy_brief.contains(UNIVERSAL_PRINCIPLES),
            "{very_easy_brief}"
        );
        for difficulty in DIFFICULTIES.into_iter().skip(1) {
            let brief = brief(FormatConfig::modern(), difficulty);
            assert!(brief.contains(MODERN), "{difficulty:?}: {brief}");
        }
    }

    #[test]
    fn the_brief_yields_to_the_difficulty_brief_on_pace() {
        let brief = brief(FormatConfig::commander(), AiDifficulty::CEDH);
        assert!(
            brief.contains("follow your playing-strength description"),
            "{brief}"
        );
    }

    #[test]
    fn commander_draft_teaches_commander_play_on_top_of_the_draft_framing() {
        let brief = brief(FormatConfig::commander_draft(), AiDifficulty::Medium);
        assert!(brief.contains(COMMANDER_DRAFT), "{brief}");
        assert!(brief.contains(COMMANDER), "{brief}");
        assert!(!brief.contains(COMMANDER_SINGLETON), "{brief}");
    }

    /// The brief is static, engine-authored text living outside the data fence, so
    /// it must not itself contain a fence marker.
    #[test]
    fn no_brief_contains_a_fence_marker() {
        for config in builtin_configs() {
            for difficulty in DIFFICULTIES {
                let brief = brief(config.clone(), difficulty);
                assert!(!brief.contains(UNTRUSTED_DATA_BEGIN), "{brief}");
                assert!(!brief.contains(UNTRUSTED_DATA_END), "{brief}");
            }
        }
    }
}
