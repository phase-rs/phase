import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuShell } from "../components/menu/MenuShell";
import { MyDecks } from "../components/menu/MyDecks";
import { useCardDataStore } from "../stores/cardDataStore";

export function MyDecksPage() {
  const navigate = useNavigate();
  const { t } = useTranslation("menu");
  useAudioContext("deck_builder");

  // Warm the shared card DB so deck compat/coverage scans below are instant.
  // Idempotent; closes the deep-link hole when opening /my-decks directly.
  useEffect(() => {
    void useCardDataStore.getState().warm();
  }, []);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow={t("myDecksPage.eyebrow")}
        title={t("myDecksPage.title")}
        description={t("myDecksPage.description")}
        layout="stacked"
      >
        <MyDecks
          mode="manage"
          onCreateDeck={() => navigate("/deck-builder?create=1&returnTo=%2Fmy-decks")}
          onEditDeck={(name) =>
            navigate(`/deck-builder?deck=${encodeURIComponent(name)}&returnTo=%2Fmy-decks`)
          }
        />
      </MenuShell>
    </div>
  );
}
