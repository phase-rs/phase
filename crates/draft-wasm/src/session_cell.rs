//! The draft session's thread-local home, and the ONLY module that can name it.
//!
//! `DRAFT_SESSION` holds the WHOLE session -- main-stack draw order and every
//! pile's contents, which are a shared-stack draft's only secrets. `bot_ai` is a
//! SIBLING of this module, not a descendant, so neither `crate::DRAFT_SESSION`
//! (no such path any more: `error[E0425]`) nor
//! `crate::session_cell::DRAFT_SESSION` (`error[E0603]`, the static is private)
//! resolves there. That is the compiler enforcing the no-cheating boundary
//! rather than a reviewer.
//!
//! What this module does NOT enforce: the `pub(crate)` accessors below ARE
//! nameable from `bot_ai`. No VISIBILITY MODIFIER closes that -- there is none
//! meaning "the crate root but not a sibling": `pub(in crate::..)` grants
//! visibility to a module *and its descendants*, and the crate root is an
//! ancestor of `session_cell`, not a descendant. So that channel is closed by a
//! test that varies the installed session while holding the bot's argument
//! fixed (T-CHEAT-1b, landing with the bot itself), and not by this module. Do
//! not add an accessor that hands out something narrower and call the problem
//! solved -- the test is the authority on that channel.
//!
//! IT IS CLOSABLE, and this is a deferral rather than a dead end, so do not
//! read the paragraph above as "nothing can be done". `bot_ai`'s production
//! half names nothing from this crate -- its whole dependency set is
//! `draft_core`, `engine`, `phase_ai` and `rand` -- so lifting it into a crate
//! of its own would make these `pub(crate)` items unnameable BY THE COMPILER,
//! the same device that already closes the other channel. That is a module
//! moving across a crate boundary, which is why it has not been done here.
//!
//! `PACK_GEN`, `DIFFICULTY`, `RNG` and `CARD_DB` deliberately stay at the crate
//! root: none of them carries hidden information, and `CARD_DB` is the PUBLIC
//! card database a bot reads through its own argument list.
//!
//! # What none of this closes: the HUMAN HOST
//!
//! Every device above is about code inside this crate. None of them — and no
//! change to this module — can stop the person running a P2P host from reading
//! their own browser's WASM heap, where this session lives in full. A P2P host
//! can recover `main_stack`'s order, and with it every pile the rest of the
//! draft will deal, without touching the engine at all.
//!
//! THIS IS THE P2P TRUST MODEL, NOT A WINSTON DEFECT, and it predates Winston:
//! the same session holds `packs_by_seat` (every seat's unopened boosters) and
//! `pools` (every seat's drafted cards) for pick-and-pass and sealed pods, so a
//! P2P host has always held every other seat's secrets. What Winston changes is
//! the DEGREE. A booster a host reads early is an advantage; a main-stack order
//! is the whole remainder of a two-player draft, deterministically, because the
//! piles are dealt off it in order.
//!
//! The answer is deployment, not code: a pod hosted by `phase-server` keeps the
//! session on the server and hands every seat — the host included — only
//! `filter_for_player`'s projection. Redaction of the backup payload
//! (`p2p_backup_guard`, `redactDraftSessionObject`) protects the row, which is a
//! different channel; it does nothing about live host memory and was never
//! claimed to. Do not read the redaction work as covering this.

use std::cell::Cell;

use wasm_bindgen::prelude::*;

use draft_core::types::DraftSession;

thread_local! {
    /// Draft session state uses Cell<Option<T>> with take/set to avoid RefCell
    /// borrow poisoning — same panic-resilient pattern as engine-wasm.
    ///
    /// NOT `pub` in any form: that is the point of this module.
    static DRAFT_SESSION: Cell<Option<DraftSession>> = const { Cell::new(None) };
}

/// Take the draft session out of the Cell, pass it to a closure, then put it back.
pub(crate) fn with_draft<R>(f: impl FnOnce(&DraftSession) -> R) -> Result<R, JsValue> {
    DRAFT_SESSION.with(|cell| {
        let session = cell
            .take()
            .ok_or_else(|| JsValue::from_str("Draft not initialized"))?;
        let result = f(&session);
        cell.set(Some(session));
        Ok(result)
    })
}

/// Take the draft session out of the Cell, pass it mutably, then put it back.
pub(crate) fn with_draft_mut<R>(
    f: impl FnOnce(&mut DraftSession) -> Result<R, JsValue>,
) -> Result<R, JsValue> {
    DRAFT_SESSION.with(|cell| {
        let mut session = cell
            .take()
            .ok_or_else(|| JsValue::from_str("Draft not initialized"))?;
        let result = f(&mut session);
        cell.set(Some(session));
        result
    })
}

/// `with_draft_mut` for the pure-Rust `_inner` cores: identical take/run/put
/// dance, but `String` errors so the core is callable from `cargo test` on a
/// native target, where every `JsValue` operation is unavailable.
pub(crate) fn with_draft_mut_inner<R>(
    f: impl FnOnce(&mut DraftSession) -> Result<R, String>,
) -> Result<R, String> {
    DRAFT_SESSION.with(|cell| {
        let mut session = cell.take().ok_or("Draft not initialized")?;
        let result = f(&mut session);
        cell.set(Some(session));
        result
    })
}

/// `with_draft` for the pure-Rust `_inner` cores: identical take/run/put dance
/// over a SHARED borrow, but `String` errors so the core is callable from
/// `cargo test` on a native target.
///
/// The shared sibling of `with_draft_mut_inner`. A read-only `_inner` core must
/// not reach for the `&mut` helper instead: taking `&mut` for a body that only
/// reads is the kind of borrow the type system is there to state honestly.
pub(crate) fn with_draft_inner<R>(
    f: impl FnOnce(&DraftSession) -> Result<R, String>,
) -> Result<R, String> {
    DRAFT_SESSION.with(|cell| {
        let session = cell.take().ok_or("Draft not initialized")?;
        let result = f(&session);
        cell.set(Some(session));
        result
    })
}

/// Install a session, replacing whatever was there. The one *write* verb: every
/// production path that used to say `DRAFT_SESSION.with(|cell| cell.set(Some(..)))`
/// says this instead, so `grep` for the session's installers has one answer.
pub(crate) fn install(session: DraftSession) {
    DRAFT_SESSION.with(|cell| cell.set(Some(session)));
}

/// Drop the installed session, if any.
#[cfg(test)]
pub(crate) fn clear() {
    DRAFT_SESSION.with(|cell| cell.set(None));
}

/// Whether a session is installed, **without disturbing it**.
///
/// The sites this replaces were spelled `assert!(cell.take().is_none())`, which
/// left the cell `None` whether or not the assertion held. This is
/// behaviour-equivalent at every one of those sites (each asserts the cell is
/// empty, so the destructive `take` had nothing to destroy) and strictly safer
/// for any future caller.
#[cfg(test)]
pub(crate) fn is_installed() -> bool {
    DRAFT_SESSION.with(|cell| {
        let session = cell.take();
        let installed = session.is_some();
        cell.set(session);
        installed
    })
}

/// Read the installed session, panicking if there is none -- the take/put dance
/// `with_draft_inner` runs, without the `Result` wrapper a test body would only
/// have to unwrap.
#[cfg(test)]
pub(crate) fn with_installed<R>(f: impl FnOnce(&DraftSession) -> R) -> R {
    DRAFT_SESSION.with(|cell| {
        let session = cell.take().expect("a draft session is installed");
        let out = f(&session);
        cell.set(Some(session));
        out
    })
}

/// `with_installed` over a mutable borrow, for fixtures that seed the installed
/// session directly.
#[cfg(test)]
pub(crate) fn with_installed_mut<R>(f: impl FnOnce(&mut DraftSession) -> R) -> R {
    DRAFT_SESSION.with(|cell| {
        let mut session = cell.take().expect("a draft session is installed");
        let out = f(&mut session);
        cell.set(Some(session));
        out
    })
}
