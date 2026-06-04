//! Wire validation for lobby subscription fan-out in `phase-server`.
//!
//! `SubscribeLobby` registers each connection's outbound sender in a shared
//! subscriber list and returns a full lobby snapshot. Without a cap, many
//! WebSocket clients can force unbounded sender storage and repeated full
//! lobby clones.

/// Server-wide cap on live lobby subscribers. Bounds memory and broadcast
/// fan-out from `SubscribeLobby` / `Outbound::AddSubscriber`.
pub const MAX_LOBBY_SUBSCRIBERS: usize = 128;

/// Reject new lobby subscriptions once the subscriber list is at capacity.
pub fn guard_lobby_subscriber_capacity(current: usize) -> Result<(), String> {
    if current >= MAX_LOBBY_SUBSCRIBERS {
        Err(format!(
            "Too many lobby subscribers: maximum is {MAX_LOBBY_SUBSCRIBERS}"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lobby_subscriber_capacity_accepts_slot_below_cap() {
        assert!(guard_lobby_subscriber_capacity(MAX_LOBBY_SUBSCRIBERS - 1).is_ok());
    }

    #[test]
    fn lobby_subscriber_capacity_rejects_slot_at_cap() {
        let err = guard_lobby_subscriber_capacity(MAX_LOBBY_SUBSCRIBERS).unwrap_err();
        assert!(err.contains("maximum"));
        assert!(err.contains(&MAX_LOBBY_SUBSCRIBERS.to_string()));
    }
}
