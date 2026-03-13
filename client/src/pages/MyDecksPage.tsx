import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuShell } from "../components/menu/MenuShell";
import { MyDecks } from "../components/menu/MyDecks";

export function MyDecksPage() {
  const navigate = useNavigate();

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow="Decks"
        title="Decks."
        description="Open a saved list, import a new one, or continue in deck builder."
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
