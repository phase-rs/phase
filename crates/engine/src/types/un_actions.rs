//! Engine-side Un-card action state: the per-game opt-in, menu and Gotcha-offer
//! bookkeeping layered over the [`phase_ast::types::un_actions`] vocabulary.
//! Offers bind to [`ObjectIdentityBinding`], which lives in the engine.

pub use phase_ast::types::un_actions::*;

use serde::{Deserialize, Serialize};

use super::identifiers::{ObjectId, ObjectIdentityBinding};
use super::player::PlayerId;

#[inline]
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// Un-card (no CR entry): what the host observed when it reported on an
/// in-flight spoken unless-payment.
///
/// A saying and the passing of time are different observations. Only a
/// [`SayPhraseReport::Saying`] can count toward the payment; a
/// [`SayPhraseReport::Expiry`] reports that the host's clock has run past the
/// window with no saying, so it can lapse the payment but never pay it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum SayPhraseReport {
    /// The payer said the phrase at the stamped instant.
    #[default]
    Saying,
    /// The host's clock read the stamped instant and no saying had arrived.
    Expiry,
}

/// Un-card (no CR entry): one host-stamped observation of an in-flight spoken
/// unless-payment.
///
/// The real-time difficulty of a say-it cost (Toy Boat's "quickly … without
/// pausing") is physical, so the engine adapts it exactly as `types::contest`
/// adapts a staring contest: the engine never reads a clock. The host that can
/// observe wall-clock time stamps what the human did with its own
/// epoch-millisecond instant and a per-payment sequence, and the deterministic
/// engine validates those stamps against the deadline it stores. Hosts use the
/// Unix-epoch domain so a stamp taken after a reload or a reconnect is still
/// comparable with the deadline the engine retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SayPhraseObservation {
    /// Monotonic per-payment sequence number of a saying. Duplicate and
    /// reordered deliveries carry nothing newer and are ignored. An expiry
    /// report carries no sequence.
    #[serde(default)]
    pub seq: u32,
    /// Host-stamped instant, in epoch milliseconds.
    pub at_ms: u64,
    /// Whether the host observed a saying or only the clock.
    #[serde(default)]
    pub report: SayPhraseReport,
}

/// Decode a host-stamped instant into the integer-millisecond wire contract.
///
/// Browser hosts read `performance.now()`-derived clocks, which carry
/// fractional milliseconds; `serde-wasm-bindgen` refuses a fractional number
/// for a `u64`, which would reject a real saying before the engine saw it. The
/// host-input boundary therefore accepts any finite, non-negative number and
/// floors it, exactly as the contest click decoder does. Negative and
/// non-finite stamps are malformed and rejected.
pub fn deserialize_host_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct HostMs;

    impl serde::de::Visitor<'_> for HostMs {
        type Value = u64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a finite, non-negative millisecond instant")
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom("a host instant cannot be negative"))
        }

        fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<u64, E> {
            if !v.is_finite() || v < 0.0 || v >= u64::MAX as f64 {
                return Err(E::custom("a host instant must be finite and non-negative"));
            }
            Ok(v.floor() as u64)
        }
    }

    deserializer.deserialize_any(HostMs)
}

/// Un-card (no CR entry): the engine-owned instance of one spoken payment.
///
/// The printed cost states how many sayings are owed (CR 702.24a raises it by
/// one per age counter); this instance records how many the engine has actually
/// accepted and the host instant by which the next one must arrive. It lives in
/// `WaitingFor::UnlessPayment`, so it is part of `GameState` and survives a
/// reconnect or a save/restore exactly like the contest state does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SayPhrasePayment {
    /// Engine-authored identity, minted once per payment. Every host
    /// observation names it, so a frame delayed from an earlier payment can
    /// never be mistaken for activity in a later one.
    pub id: u64,
    /// Sayings the printed cost requires in total (CR 702.24a expansion has
    /// already happened when this is minted).
    pub owed: u32,
    /// Sayings the engine has accepted so far.
    #[serde(default)]
    pub said: u32,
    /// Highest host instant observed. Time never runs backwards, even if the
    /// host delivers stamps out of order.
    #[serde(default)]
    pub clock_ms: u64,
    /// Highest accepted sequence, for duplicate/reorder protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<u32>,
    /// Host instant of the last accepted saying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_said_ms: Option<u64>,
    /// Host instant by which the next saying must arrive. `None` until the
    /// first saying: the pace times the gap BETWEEN sayings, so reading the
    /// prompt before the first one is never a pause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

/// How a host observation was classified by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SayPhraseDisposition {
    /// Recorded as a saying made inside the window.
    Accepted,
    /// Duplicate or out-of-order delivery, or an expiry report that arrived
    /// before the window closed: the payment's progress does not move, exactly
    /// as a stale contest click is ignored.
    Stale,
    /// Arrived after `deadline_ms`: the payer paused, so the cost is not paid
    /// (CR 118.12a).
    Lapsed,
}

impl SayPhrasePayment {
    /// A freshly surfaced payment that requires `owed` sayings.
    pub fn new(id: u64, owed: u32) -> Self {
        Self {
            id,
            owed: owed.max(1),
            said: 0,
            clock_ms: 0,
            last_seq: None,
            last_said_ms: None,
            deadline_ms: None,
        }
    }

    /// Sayings still owed.
    pub fn remaining(&self) -> u32 {
        self.owed.saturating_sub(self.said).max(1)
    }

    /// Classify and record one host observation.
    ///
    /// `max_gap_ms` is the pace's published window (Toy Boat's "quickly");
    /// `None` means the phrase may be said at the payer's own pace, so only
    /// duplicate/reorder protection applies. A saying is a duplicate when its
    /// sequence is not newer than the last accepted one, exactly as
    /// `game::contest::ContestState::click` rejects a stale click. A client that
    /// reconnects mid-payment reads `last_seq` from this published instance and
    /// continues from it, so a restart cannot replay an accepted saying.
    ///
    /// Every comparison uses the payment's retained clock — the highest instant
    /// any observation has reported — never the raw stamp alone, so a lower
    /// stamp delivered late cannot be accepted against a deadline the payment's
    /// clock has already passed.
    ///
    /// The deadline is inclusive: a saying stamped exactly at it is in time.
    /// An expiry report therefore lapses the payment only once the clock is
    /// strictly past the deadline, and it never counts as a saying.
    pub fn observe(
        &mut self,
        observation: SayPhraseObservation,
        max_gap_ms: Option<u64>,
    ) -> SayPhraseDisposition {
        if observation.report == SayPhraseReport::Saying
            && self
                .last_seq
                .is_some_and(|last_seq| observation.seq <= last_seq)
        {
            return SayPhraseDisposition::Stale;
        }
        self.clock_ms = self.clock_ms.max(observation.at_ms);
        // CR 118.12a: the window is part of the payment. Once the clock is past
        // the stored deadline the payer has paused, which leaves the cost unpaid.
        if self
            .deadline_ms
            .is_some_and(|deadline| self.clock_ms > deadline)
        {
            return SayPhraseDisposition::Lapsed;
        }
        match observation.report {
            // The clock has not yet passed the window: nothing to decide.
            SayPhraseReport::Expiry => SayPhraseDisposition::Stale,
            SayPhraseReport::Saying => {
                self.said = self.said.saturating_add(1);
                self.last_seq = Some(observation.seq);
                self.last_said_ms = Some(self.clock_ms);
                self.deadline_ms = max_gap_ms.map(|gap| self.clock_ms.saturating_add(gap));
                SayPhraseDisposition::Accepted
            }
        }
    }
}

/// A live opportunity for `player` to call "Gotcha!" for the Gotcha ability on
/// `source`, returning `returns` to hand when the reflexive trigger resolves.
///
/// `source` and `returns` are each bound to the object's occurrence (object id +
/// incarnation) and the zone it was in when the window opened. CR 400.7: an object
/// that changes zones becomes a new object and bumps its incarnation, so a bare
/// ObjectId goes stale — the bindings let `offer_is_still_valid` reject a window
/// whose source or returned card has moved or been replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GotchaOffer {
    pub id: u64,
    pub player: PlayerId,
    /// The object carrying the Gotcha ability.
    pub source: ObjectIdentityBinding,
    /// The card the ability returns ("this card" → `source`; "that card" → the
    /// graveyard card whose name contained the spoken word).
    pub returns: ObjectIdentityBinding,
}

/// Per-game Un-card action state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnActionState {
    /// Players who opted in to Un-card actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_players: Vec<PlayerId>,
    /// Gesture kinds worth offering in the Un-actions menu, derived by
    /// `game::un_actions::refresh_menu` after every action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<UnTellKind>,
    /// Gotcha windows opened by the most recent [`UnTell`]. Cleared by the next
    /// ordinary action, so a window never outlives the moment it was offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gotcha_offers: Vec<GotchaOffer>,
    /// Gotcha offers already matched by a gesture made while not at a priority
    /// boundary, waiting to be announced when priority is next reached. One per
    /// (player, source, returned card), so the list is bounded by the cards in play.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_offers: Vec<GotchaOffer>,
    /// Next unique identifier minted for a [`GotchaOffer`].
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub next_offer_id: u64,
    /// Next unique identifier minted for a [`SayPhrasePayment`]. The identity
    /// outlives a reconnect with the rest of `GameState`, so a delayed frame
    /// stays bound to the payment it was observed in.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub next_say_payment_id: u64,
}

impl UnActionState {
    pub fn mint_offer_id(&mut self) -> u64 {
        self.next_offer_id += 1;
        self.next_offer_id
    }

    pub fn mint_say_payment_id(&mut self) -> u64 {
        self.next_say_payment_id += 1;
        self.next_say_payment_id
    }

    pub fn is_enabled(&self, player: PlayerId) -> bool {
        self.enabled_players.contains(&player)
    }

    pub fn set_enabled(&mut self, player: PlayerId, enabled: bool) {
        self.enabled_players.retain(|p| *p != player);
        if enabled {
            self.enabled_players.push(player);
        }
    }

    pub fn is_default(&self) -> bool {
        self.enabled_players.is_empty()
            && self.gotcha_offers.is_empty()
            && self.menu.is_empty()
            && self.pending_offers.is_empty()
            && self.next_offer_id == 0
            && self.next_say_payment_id == 0
    }
}

/// The payload of `GameAction::UnAction`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum UnAction {
    /// Opt the acting player in or out of Un-card actions (a preference).
    SetEnabled { enabled: bool },
    /// Perform a gesture from the Un-actions menu.
    PerformTell { tell: UnTell },
    /// Call "Gotcha!" on an open offer.
    SayGotcha {
        source_id: ObjectId,
        returns: ObjectId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_set_toggles_idempotently() {
        let mut state = UnActionState::default();
        state.set_enabled(PlayerId(1), true);
        state.set_enabled(PlayerId(1), true);
        assert_eq!(state.enabled_players, vec![PlayerId(1)]);
        state.set_enabled(PlayerId(1), false);
        assert!(state.is_default());
    }

    /// The spoken-payment allocator outlives the last opt-out: an otherwise
    /// default state with a minted payment id is not default, so `GameState`
    /// keeps it on save and a retired id is never minted again.
    #[test]
    fn say_payment_allocator_survives_a_round_trip() {
        let mut state = UnActionState::default();
        state.set_enabled(PlayerId(0), true);
        assert_eq!(state.mint_say_payment_id(), 1);
        state.set_enabled(PlayerId(0), false);
        assert!(!state.is_default());

        let wire = serde_json::to_string(&state).expect("state serializes");
        let mut restored: UnActionState = serde_json::from_str(&wire).expect("state deserializes");
        assert_eq!(restored.next_say_payment_id, 1);
        assert_eq!(restored.mint_say_payment_id(), 2);
    }

    /// A fractional host clock is floored into the integer wire contract; a
    /// negative or non-numeric stamp is malformed.
    #[test]
    fn host_instants_decode_into_integer_milliseconds() {
        #[derive(Deserialize)]
        struct Stamp {
            #[serde(deserialize_with = "deserialize_host_ms")]
            at_ms: u64,
        }
        let decode = |json: &str| serde_json::from_str::<Stamp>(json).map(|s| s.at_ms);
        assert_eq!(
            decode(r#"{"at_ms":1730937600123.875}"#).unwrap(),
            1_730_937_600_123
        );
        assert_eq!(decode(r#"{"at_ms":42}"#).unwrap(), 42);
        assert!(decode(r#"{"at_ms":-1}"#).is_err());
        assert!(decode(r#"{"at_ms":-0.5}"#).is_err());
        assert!(decode(r#"{"at_ms":"soon"}"#).is_err());
    }

    fn saying(seq: u32, at_ms: u64) -> SayPhraseObservation {
        SayPhraseObservation {
            seq,
            at_ms,
            report: SayPhraseReport::Saying,
        }
    }

    fn expiry(at_ms: u64) -> SayPhraseObservation {
        SayPhraseObservation {
            seq: 0,
            at_ms,
            report: SayPhraseReport::Expiry,
        }
    }

    /// An expiry report is never a saying: at the inclusive deadline it changes
    /// nothing, and only once the clock is past it does it lapse the payment.
    #[test]
    fn an_expiry_report_lapses_but_never_pays() {
        let mut payment = SayPhrasePayment::new(1, 2);
        assert_eq!(
            payment.observe(expiry(500), Some(1_000)),
            SayPhraseDisposition::Stale,
            "before the first saying there is no window to expire"
        );
        assert_eq!(
            payment.observe(saying(1, 1_000), Some(1_000)),
            SayPhraseDisposition::Accepted
        );
        assert_eq!(
            payment.observe(expiry(2_000), Some(1_000)),
            SayPhraseDisposition::Stale,
            "the deadline itself is still in time"
        );
        assert_eq!(payment.said, 1, "an expiry report is not a saying");
        assert_eq!(
            payment.observe(saying(2, 2_000), Some(1_000)),
            SayPhraseDisposition::Accepted,
            "a saying at the inclusive deadline is in time"
        );

        let mut late = SayPhrasePayment::new(2, 2);
        late.observe(saying(1, 1_000), Some(1_000));
        assert_eq!(
            late.observe(expiry(2_001), Some(1_000)),
            SayPhraseDisposition::Lapsed
        );
        assert_eq!(late.said, 1, "the lapse did not count a saying");
    }

    /// A saying stamped lower than the payment's retained clock (a stamp from
    /// another time origin, or a frame delayed behind an expiry check) is judged
    /// at the retained clock, so it cannot slip in under a passed deadline.
    #[test]
    fn a_lower_stamp_is_judged_at_the_retained_clock() {
        let mut payment = SayPhrasePayment::new(1, 3);
        payment.observe(saying(1, 10_000), Some(1_000));
        assert_eq!(
            payment.observe(expiry(10_900), Some(1_000)),
            SayPhraseDisposition::Stale
        );
        assert_eq!(
            payment.observe(saying(2, 3), Some(1_000)),
            SayPhraseDisposition::Accepted
        );
        assert_eq!(
            payment.deadline_ms,
            Some(11_900),
            "the window restarts from the retained clock, not the raw stamp"
        );
        assert_eq!(
            payment.observe(expiry(12_500), Some(1_000)),
            SayPhraseDisposition::Lapsed
        );
        let mut after_lapse_clock = SayPhrasePayment::new(2, 2);
        after_lapse_clock.observe(saying(1, 10_000), Some(1_000));
        assert_eq!(
            after_lapse_clock.observe(saying(2, 20_000), Some(1_000)),
            SayPhraseDisposition::Lapsed
        );
        assert_eq!(
            after_lapse_clock.observe(saying(3, 10_500), Some(1_000)),
            SayPhraseDisposition::Lapsed,
            "a lower stamp after the clock passed the deadline is still a pause"
        );
    }
}
