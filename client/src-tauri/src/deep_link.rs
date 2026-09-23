//! `phase://` desktop deep links: the one intake that turns a link handed to
//! the shell into a first-party page for the web app to open.
//!
//! Grammar, as the card-bot emits it:
//!
//! ```text
//! phase://open?site=<release|preview>&path=<urlencoded /multiplayer?…>
//! ```
//!
//! `site` is a [`Channel`] by its serde name and `path` is the web link's
//! path and query. The link never supplies a scheme, host or origin: the
//! destination origin is always [`Channel::origin`], a compile-time constant,
//! because the remote origins hold the `remote-shell-*` capabilities. The path
//! must start with `/multiplayer?`, which forces a path-absolute reference with
//! a literal `/multiplayer` segment and so excludes scheme-relative
//! (`//host`), absolute (`https://…`) and other-route references; the joined
//! result must carry no fragment.
//!
//! Everything after `/multiplayer?` is attacker-controlled. That is the trust
//! level of any plain web link to `https://<site>/multiplayer?…`, which anyone
//! can already post, so the query is not interpreted here: the web app's
//! arrival handling owns its validation. This module only guarantees that the
//! query lands on a first-party constant origin.
//!
//! Raw-URL visibility: the plugin broadcasts every raw, unvalidated link on
//! `deep-link://new-url` to every webview, and any first-party page holding
//! `core:default` can listen to it or emit it. Nothing here hides the raw
//! link. The contract is that no consumer acts on it: the bootstrap and the
//! web app act only on what [`take_pending_deep_link`] returns, which is the
//! output of [`validate_deep_link`], and a page-emitted event reaches
//! [`deliver`] and is validated like any other. The link carries no secret
//! beyond the game code, which the page can read from its own URL on arrival.
//!
//! The dev shell is unsupported: macOS registers the scheme only for an
//! installed bundle, and the destination is always a production channel
//! origin, never the dev server or a `SHELL_REMOTE_ORIGIN` override. A debug
//! build never registers the Linux handler either: it would register
//! `target/debug/phase-tauri`, and an installed build's `is_registered`
//! pre-check would then skip its own registration for good.

use std::{borrow::Cow, fmt::Display, sync::Mutex};

use serde::{de::value::StrDeserializer, Deserialize};
use tauri::{AppHandle, Emitter, Url};
use tauri_plugin_deep_link::DeepLinkExt;

use crate::{channels::Channel, update_authority::UpdateAuthority};

/// The URL scheme the shell registers; `tauri.conf.json` declares the same.
pub const DEEP_LINK_SCHEME: &str = "phase";
/// Payload-free signal that a validated link is waiting to be taken.
const PENDING_EVENT: &str = "deep-link-pending";
const MULTIPLAYER_PREFIX: &str = "/multiplayer?";

/// Maps a `phase://open?site=…&path=…` link to the first-party page it names,
/// or `None` for any other shape.
pub fn validate_deep_link(link: &Url) -> Option<Url> {
    let outer_shape = link.scheme() == DEEP_LINK_SCHEME
        && link.host_str() == Some("open")
        && link.port().is_none()
        && link.username().is_empty()
        && link.password().is_none()
        && matches!(link.path(), "" | "/")
        && link.fragment().is_none();
    if !outer_shape {
        return None;
    }

    let mut site: Option<Cow<str>> = None;
    let mut path: Option<Cow<str>> = None;
    for (key, value) in link.query_pairs() {
        let slot = match key.as_ref() {
            "site" => &mut site,
            "path" => &mut path,
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    let (site, path) = (site?, path?);

    let channel =
        Channel::deserialize(StrDeserializer::<serde::de::value::Error>::new(&site)).ok()?;
    if !path.starts_with(MULTIPLAYER_PREFIX) {
        return None;
    }
    let target = Url::parse(channel.origin()).ok()?.join(&path).ok()?;
    target.fragment().is_none().then_some(target)
}

/// The one link waiting to be taken: latest valid wins, taken once.
struct PendingDeepLink(Mutex<Option<Url>>);

impl PendingDeepLink {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Stores the last valid link of a delivery and reports whether it stored
    /// one. A delivery with no valid link leaves the slot untouched, so an
    /// invalid link can never evict a valid one.
    fn offer(&self, links: impl IntoIterator<Item = Url>) -> bool {
        let Some(link) = links
            .into_iter()
            .filter_map(|link| validate_deep_link(&link))
            .last()
        else {
            return false;
        };
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(link);
            true
        } else {
            false
        }
    }

    fn take(&self) -> Option<Url> {
        self.0.lock().ok()?.take()
    }
}

static PENDING: PendingDeepLink = PendingDeepLink::new();

/// Every entry point — cold-start arguments, a second instance, a macOS
/// Apple Event, a page-emitted event — converges here. Rust never navigates:
/// it fills the slot, brings the window forward and lets the page pull.
pub fn deliver(app: &AppHandle, links: Vec<Url>) {
    if PENDING.offer(links) {
        crate::focus_main_window(app);
        let _ = app.emit(PENDING_EVENT, ());
    }
}

/// Returns and clears the validated link the shell holds.
#[tauri::command]
pub fn take_pending_deep_link() -> Option<Url> {
    PENDING.take()
}

/// Whether the shell should write its own `phase:` handler. Flatpak never
/// does; otherwise only when the handler is known to be missing, and a failed
/// query skips rather than guesses. The query is a closure so the Flatpak arm
/// never spawns `xdg-mime`.
fn registration_needed<E: Display>(
    authority: UpdateAuthority,
    is_registered: impl FnOnce() -> Result<bool, E>,
) -> bool {
    match authority {
        UpdateAuthority::Flatpak => false,
        UpdateAuthority::Shell => match is_registered() {
            Ok(registered) => !registered,
            Err(error) => {
                eprintln!(
                    "deep link: cannot query the {DEEP_LINK_SCHEME}: handler ({error}); skipping registration"
                );
                false
            }
        },
    }
}

/// Registers the `phase:` handler on Linux when it is missing.
///
/// tauri-bundler's deb/rpm/AppImage `.desktop` declares
/// `MimeType=x-scheme-handler/phase` but its `Exec` lacks `%u`
/// (tauri-apps/tauri#15928), so the link never reaches the process; the
/// plugin's own `register_all` writes `<exe>-handler.desktop` with `%u`.
/// Flatpak is skipped: the sandbox cannot reach the host's `xdg-mime`, and the
/// exported `rs.phase.app.desktop` already carries the handler.
///
/// Limitation: `register` itself rewrites a stale `Exec` line, but it is only
/// reached when the `is_registered` pre-check here reports the handler
/// missing, and that check only asks whether `xdg-mime` names
/// `<exe>-handler.desktop`, not whether that file's `Exec` still points at this
/// executable. An AppImage moved after its first launch therefore keeps a
/// stale `Exec` until the user deletes
/// `~/.local/share/applications/<exe>-handler.desktop`.
///
/// Only a Linux release build calls this: on macOS the plugin returns
/// `UnsupportedPlatform`, and on Windows the installer registers the scheme.
/// A debug build never calls it (see the module docs on the dev shell).
#[cfg_attr(any(not(target_os = "linux"), debug_assertions), allow(dead_code))]
pub fn register_scheme_if_missing(app: AppHandle) {
    if registration_needed(UpdateAuthority::detect(), || {
        app.deep_link().is_registered(DEEP_LINK_SCHEME)
    }) {
        if let Err(error) = app.deep_link().register_all() {
            eprintln!("deep link: cannot register the {DEEP_LINK_SCHEME}: handler ({error})");
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    const BASE_RELEASE: &str = "phase://open?site=release&path=%2Fmultiplayer%3Fjoin%3DAB12CD%2540wss%253A%252F%252Flobby.phase-rs.dev%252Fws";
    const BASE_PREVIEW: &str = "phase://open?site=preview&path=%2Fmultiplayer%3Fjoin%3DAB12CD%2540wss%253A%252F%252Flobby.phase-rs.dev%252Fws";
    const JOIN_QUERY: &str = "/multiplayer?join=AB12CD%40wss%3A%2F%2Flobby.phase-rs.dev%2Fws";

    fn parse(link: &str) -> Url {
        Url::parse(link).unwrap()
    }

    /// Builds a link the way the bot does (`URLSearchParams`).
    fn bot_link(site: &str, path: &str) -> Url {
        let mut link = parse("phase://open");
        link.query_pairs_mut()
            .append_pair("site", site)
            .append_pair("path", path);
        link
    }

    #[test]
    fn a_valid_link_maps_to_its_channel_origin_and_multiplayer_query() {
        assert_eq!(
            validate_deep_link(&parse(BASE_RELEASE)),
            Some(parse(&format!("https://phase-rs.dev{JOIN_QUERY}")))
        );
        assert_eq!(
            validate_deep_link(&parse(BASE_PREVIEW)),
            Some(parse(&format!("https://preview.phase-rs.dev{JOIN_QUERY}")))
        );
        assert_eq!(bot_link("release", JOIN_QUERY), parse(BASE_RELEASE));
        assert_eq!(
            validate_deep_link(&parse(&BASE_RELEASE.replacen("open?", "open/?", 1))),
            Some(parse(&format!("https://phase-rs.dev{JOIN_QUERY}")))
        );

        let host = "/multiplayer?code=AB12CD&format=commander&players=4&room=Discord+Commander";
        let taken = validate_deep_link(&bot_link("release", host)).unwrap();
        assert_eq!(taken.as_str(), format!("https://phase-rs.dev{host}"));
        assert_eq!(
            taken
                .query_pairs()
                .find(|(key, _)| key == "room")
                .map(|(_, value)| value.into_owned()),
            Some("Discord Commander".to_owned())
        );
    }

    #[test]
    fn every_other_shape_is_rejected() {
        assert!(validate_deep_link(&parse(BASE_RELEASE)).is_some());
        let path =
            "path=%2Fmultiplayer%3Fjoin%3DAB12CD%2540wss%253A%252F%252Flobby.phase-rs.dev%252Fws";
        let with_path = |site_and_path: &str| format!("phase://open?{site_and_path}");
        let with_site_path =
            |path_value: &str| format!("phase://open?site=release&path={path_value}");
        let rows: Vec<(&str, String)> = vec![
            ("scheme https", BASE_RELEASE.replacen("phase:", "https:", 1)),
            ("host evil", BASE_RELEASE.replacen("//open", "//evil", 1)),
            ("host OPEN", BASE_RELEASE.replacen("//open", "//OPEN", 1)),
            ("port", BASE_RELEASE.replacen("//open", "//open:1", 1)),
            (
                "username",
                BASE_RELEASE.replacen("//open", "//user@open", 1),
            ),
            (
                "password",
                BASE_RELEASE.replacen("//open", "//user:pw@open", 1),
            ),
            ("outer path", BASE_RELEASE.replacen("open?", "open/x?", 1)),
            ("outer fragment", format!("{BASE_RELEASE}#frag")),
            (
                "site app",
                BASE_RELEASE.replacen("site=release", "site=app", 1),
            ),
            (
                "site Release",
                BASE_RELEASE.replacen("site=release", "site=Release", 1),
            ),
            ("site missing", with_path(path)),
            (
                "site duplicate",
                with_path(&format!("site=release&site=release&{path}")),
            ),
            ("path missing", "phase://open?site=release".to_owned()),
            ("path duplicate", format!("{BASE_RELEASE}&{path}")),
            ("path /", with_site_path("%2F")),
            ("path /multiplayer", with_site_path("%2Fmultiplayer")),
            (
                "path /multiplayerx?a",
                with_site_path("%2Fmultiplayerx%3Fa"),
            ),
            ("path /game/1?a", with_site_path("%2Fgame%2F1%3Fa")),
            (
                "path //evil.com/multiplayer?a",
                with_site_path("%2F%2Fevil.com%2Fmultiplayer%3Fa"),
            ),
            (
                "path https://evil/multiplayer?a",
                with_site_path("https%3A%2F%2Fevil%2Fmultiplayer%3Fa"),
            ),
            (
                "path %2F%2Fevil encoded twice",
                with_site_path("%252F%252Fevil.com%252Fmultiplayer%253Fa"),
            ),
            (
                "path /multiplayer?a#f",
                with_site_path("%2Fmultiplayer%3Fa%23f"),
            ),
            (
                "extra key origin",
                format!("{BASE_RELEASE}&origin=https%3A%2F%2Fevil"),
            ),
        ];
        // Every accepted row is reported, not just the first.
        let accepted: Vec<&str> = rows
            .iter()
            .filter(|(_, link)| validate_deep_link(&parse(link)).is_some())
            .map(|(label, _)| *label)
            .collect();
        assert_eq!(accepted, Vec::<&str>::new());
    }

    #[test]
    fn the_slot_keeps_the_latest_valid_link_and_is_taken_once() {
        let a = parse(BASE_RELEASE);
        let b = parse(BASE_PREVIEW);
        let invalid = parse("phase://evil?site=release");
        let a_target = validate_deep_link(&a);
        let b_target = validate_deep_link(&b);

        let slot = PendingDeepLink::new();
        assert!(slot.offer([a.clone()]));
        assert!(slot.offer([b.clone()]));
        assert_eq!(slot.take(), b_target);
        assert_eq!(slot.take(), None);

        // Latest wins within one delivery too.
        assert!(slot.offer([a.clone(), b.clone()]));
        assert_eq!(slot.take(), b_target);

        assert!(slot.offer([a.clone(), invalid.clone()]));
        assert_eq!(slot.take(), a_target);

        // An invalid delivery never evicts a valid link.
        assert!(slot.offer([a.clone()]));
        assert!(!slot.offer([invalid]));
        assert_eq!(slot.take(), a_target);

        assert!(!slot.offer([]));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn the_scheme_matches_the_shell_and_flatpak_config() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["plugins"]["deep-link"]["desktop"]["schemes"],
            serde_json::json!([DEEP_LINK_SCHEME])
        );

        let desktop = include_str!("../../../packaging/flatpak/rs.phase.app.desktop");
        let lines: Vec<&str> = desktop.lines().map(str::trim).collect();
        assert!(lines.contains(&format!("MimeType=x-scheme-handler/{DEEP_LINK_SCHEME};").as_str()));
        let exec: Vec<&&str> = lines
            .iter()
            .filter(|line| line.starts_with("Exec="))
            .collect();
        assert_eq!(exec.len(), 1);
        assert!(exec[0].ends_with(" %u"), "{}", exec[0]);
    }

    #[test]
    fn linux_registers_only_when_the_handler_is_missing_and_never_under_flatpak() {
        assert!(registration_needed(
            UpdateAuthority::Shell,
            || -> Result<bool, String> { Ok(false) }
        ));
        assert!(!registration_needed(
            UpdateAuthority::Shell,
            || -> Result<bool, String> { Ok(true) }
        ));
        assert!(!registration_needed(
            UpdateAuthority::Shell,
            || -> Result<bool, String> { Err("no xdg-mime".into()) }
        ));
        assert!(!registration_needed(
            UpdateAuthority::Flatpak,
            || -> Result<bool, String> { panic!("Flatpak must not query xdg-mime") }
        ));
    }
}
