//! One admission follows a command from its raw frame through handler completion.
//! Both bounded queues share these limits; decoding does not replenish capacity.

use std::sync::Arc;

use parking_lot::Mutex;

pub(crate) const MAX_COMMANDS: usize = 64;
pub(crate) const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) commands: usize,
    pub(crate) payload_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    Commands,
    PayloadBytes,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct IngressBudget(Arc<Mutex<Usage>>);

impl IngressBudget {
    pub(crate) fn admit(&self, payload_bytes: usize) -> Result<Admission, Limit> {
        let mut usage = self.0.lock();
        if usage.commands >= MAX_COMMANDS {
            return Err(Limit::Commands);
        }
        if payload_bytes > MAX_PAYLOAD_BYTES - usage.payload_bytes {
            return Err(Limit::PayloadBytes);
        }
        usage.commands += 1;
        usage.payload_bytes += payload_bytes;
        Ok(Admission {
            budget: self.clone(),
            payload_bytes,
        })
    }

    pub(crate) fn usage(&self) -> Usage {
        *self.0.lock()
    }
}

#[derive(Debug)]
pub(crate) struct Admission {
    budget: IngressBudget,
    payload_bytes: usize,
}

impl Admission {
    /// Charge the uncompressed JSON retained by a decoded command, not just
    /// its smaller gzip envelope. The command keeps its original count slot.
    pub(crate) fn resize(&mut self, payload_bytes: usize) -> Result<(), Limit> {
        let mut usage = self.budget.0.lock();
        let retained_elsewhere = usage.payload_bytes - self.payload_bytes;
        if payload_bytes > MAX_PAYLOAD_BYTES - retained_elsewhere {
            return Err(Limit::PayloadBytes);
        }
        usage.payload_bytes = retained_elsewhere + payload_bytes;
        self.payload_bytes = payload_bytes;
        Ok(())
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut usage = self.budget.0.lock();
        usage.commands -= 1;
        usage.payload_bytes -= self.payload_bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_capacity_is_not_replenished_by_moving_a_command_between_queues() {
        let budget = IngressBudget::default();
        let mut raw = Vec::with_capacity(MAX_COMMANDS);
        for _ in 0..MAX_COMMANDS {
            raw.push(budget.admit(32).unwrap());
        }
        let mut decoded = Vec::with_capacity(MAX_COMMANDS);
        while let Some(command) = raw.pop() {
            decoded.push(command);
        }
        assert_eq!(budget.admit(1).unwrap_err(), Limit::Commands);
        drop(decoded.pop());
        let replacement = budget.admit(32).unwrap();
        assert_eq!(
            budget.usage(),
            Usage {
                commands: MAX_COMMANDS,
                payload_bytes: MAX_COMMANDS * 32,
            }
        );
        drop(replacement);
        drop(decoded);
        assert_eq!(budget.usage(), Usage::default());
    }

    #[test]
    fn compressed_expansion_cannot_exceed_the_shared_retained_payload_budget() {
        let budget = IngressBudget::default();
        let mut queued = Vec::with_capacity(16);
        for _ in 0..15 {
            queued.push(budget.admit(64 * 1024).unwrap());
        }
        let mut compressed = budget.admit(1).unwrap();
        compressed.resize(64 * 1024).unwrap();
        assert_eq!(budget.admit(1).unwrap_err(), Limit::PayloadBytes);
        assert_eq!(compressed.resize(64 * 1024 + 1), Err(Limit::PayloadBytes));
        assert_eq!(budget.usage().payload_bytes, MAX_PAYLOAD_BYTES);
        drop(queued.pop());
        compressed.resize(64 * 1024 + 1).unwrap();
        assert_eq!(budget.usage().payload_bytes, 15 * 64 * 1024 + 1);
    }
}
