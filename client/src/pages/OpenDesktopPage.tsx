import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Link, useLocation, useNavigate, useSearchParams } from "react-router";

import { GITHUB_URL, social } from "../components/chrome/socialLinks";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { MenuPanel } from "../components/menu/MenuShell";

/** The only `to` this page hands to the browser: the desktop shell's link grammar. */
const DESKTOP_LINK_PREFIX = "phase://open?";
/** The only in-app route the browser fallback may enter (the bot-link arrival). */
const MULTIPLAYER_PREFIX = "/multiplayer?";
const DESKTOP_DOWNLOAD_URL = `${GITHUB_URL}/releases/latest`;

/**
 * `/open-desktop?to=<phase://open?…>` — the https target of the bot's "Open in
 * desktop app" button (Discord link buttons must be http(s)). It hands `to` to
 * the OS once; the shell validates it again, so this page only refuses anything
 * outside the `phase://open?` grammar.
 *
 * With no desktop app installed, what happens is browser-specific and not
 * controllable from here: some browsers ignore the unknown scheme and leave
 * this page (and its fallbacks) visible, others show their own error page, and
 * the user must go Back to reach the fallbacks. The hand-off therefore assigns
 * rather than replaces, so this page's history entry survives for that Back.
 *
 * Before handing off, the page marks its own history entry
 * (`desktopHandOff` in the router's location state). Entry state survives both
 * a Back that reloads the document and an in-app Back (after "Continue in
 * browser"), so an entry that already handed off never does so automatically
 * again and cannot loop back to the error page; the "Open desktop app" link
 * remains the manual retry.
 */
export function OpenDesktopPage() {
  const { t } = useTranslation("multiplayer");
  const [searchParams] = useSearchParams();
  const location = useLocation();
  const navigate = useNavigate();
  const handedOff = location.state?.desktopHandOff === true;
  const to = searchParams.get("to");
  const desktopLink = to?.startsWith(DESKTOP_LINK_PREFIX) ? to : null;
  const webPath = desktopLink === null ? null : new URL(desktopLink).searchParams.get("path");
  const browserPath = webPath?.startsWith(MULTIPLAYER_PREFIX) ? webPath : null;

  useEffect(() => {
    if (desktopLink === null || handedOff) return;
    navigate(location, { replace: true, state: { ...location.state, desktopHandOff: true } });
    window.location.assign(desktopLink);
  }, [desktopLink, handedOff, location, navigate]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col items-center justify-center px-4">
      <MenuPanel className="flex w-full max-w-md flex-col gap-4">
        {desktopLink === null ? (
          <p className="text-sm text-red-300">{t("openDesktop.invalid")}</p>
        ) : (
          <>
            <h1 className="text-xl font-semibold text-white">{t("openDesktop.title")}</h1>
            {/* A user-gesture retry for when the browser's "open app" prompt was dismissed. */}
            <a href={desktopLink} className={menuButtonClass({ tone: "emerald", size: "sm" })}>
              {t("openDesktop.openApp")}
            </a>
            {browserPath !== null && (
              <Link to={browserPath} className={menuButtonClass({ tone: "neutral", size: "sm" })}>
                {t("openDesktop.continueInBrowser")}
              </Link>
            )}
          </>
        )}
        <a
          href={DESKTOP_DOWNLOAD_URL}
          onClick={social(DESKTOP_DOWNLOAD_URL)}
          className={menuButtonClass({ tone: "neutral", size: "sm", ghost: true })}
        >
          {t("openDesktop.getDesktopApp")}
        </a>
      </MenuPanel>
    </div>
  );
}
