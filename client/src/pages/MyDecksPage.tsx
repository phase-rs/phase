import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MyDecks } from "../components/menu/MyDecks";

export function MyDecksPage() {
  const navigate = useNavigate();

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />

      <div className="relative z-10 flex w-full justify-center py-8">
        <MyDecks
          mode="manage"
          onCreateDeck={() => navigate("/deck-builder?create=1&returnTo=%2Fmy-decks")}
          onEditDeck={(name) =>
            navigate(`/deck-builder?deck=${encodeURIComponent(name)}&returnTo=%2Fmy-decks`)
          }
        />
      </div>
    </div>
  );
}
