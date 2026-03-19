import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";
import { MenuParticles } from "../components/menu/MenuParticles";

export function CoveragePage() {
  const navigate = useNavigate();

  return (
    <div className="relative flex min-h-screen flex-col overflow-hidden bg-gray-950">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="relative z-10 flex min-h-0 flex-1 flex-col">
        <CardCoverageDashboard />
      </div>
    </div>
  );
}
