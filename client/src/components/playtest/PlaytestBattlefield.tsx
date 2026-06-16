/**
 * Solitaire battlefield display. Shows all permanents with tapped/untapped state.
 * Right-clicking or clicking a permanent opens a context menu with tap/untap,
 * destroy, exile, bounce options.
 */
import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { PlaytestPermanent } from "../../services/playtestSession";
import { isLandType, cardColorClass } from "../../services/playtestSession";

type BattlefieldAction = "tap" | "untap" | "destroy" | "exile" | "bounce";

interface BfContextMenu {
  actions: Array<{ action: BattlefieldAction; labelKey: string; color: string }>;
  onClose: () => void;
  onAction: (a: BattlefieldAction) => void;
}

function BfMenu({ actions, onClose, onAction }: BfContextMenu) {
  const { t } = useTranslation("playtest");
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    function h(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", h);
    return () => document.removeEventListener("mousedown", h);
  }, [onClose]);

  return (
    <div
      ref={ref}
      className="absolute bottom-full left-1/2 z-50 mb-1 w-32 -translate-x-1/2 rounded-xl border border-white/10 bg-gray-900 py-1 shadow-xl"
    >
      {actions.map((entry) => (
        <button
          key={entry.action}
          type="button"
          onClick={() => { onAction(entry.action); onClose(); }}
          className={`w-full px-3 py-1.5 text-left text-xs hover:bg-white/8 ${entry.color}`}
        >
          {t(entry.labelKey)}
        </button>
      ))}
    </div>
  );
}

function BfCard({
  perm,
  onAction,
}: {
  perm: PlaytestPermanent;
  onAction: (action: BattlefieldAction, id: number) => void;
}) {
  const { t } = useTranslation("playtest");
  const [menuOpen, setMenuOpen] = useState(false);
  const types = perm.card.face.cardType?.coreTypes ?? [];
  const isLand = isLandType(types);

  const menuActions: Array<{ action: BattlefieldAction; labelKey: string; color: string }> = [
    perm.tapped
      ? { action: "untap",   labelKey: "bf.untap",   color: "text-blue-300" }
      : { action: "tap",     labelKey: "bf.tap",     color: "text-slate-300" },
    { action: "destroy", labelKey: "bf.destroy", color: "text-red-400" },
    { action: "exile",   labelKey: "bf.exile",   color: "text-purple-400" },
    { action: "bounce",  labelKey: "bf.bounce",  color: "text-amber-300" },
  ];

  return (
    <div className="relative">
      <button
        type="button"
        onClick={() => setMenuOpen((v) => !v)}
        title={perm.card.face.name}
        className={`flex h-24 w-16 flex-col overflow-hidden rounded-xl border shadow-md transition-all ${
          perm.tapped
            ? "border-slate-600/60 opacity-70"
            : "border-white/12 hover:border-white/30"
        } bg-gradient-to-b ${
          isLand ? "from-amber-900/50 to-amber-950/70" : "from-slate-800/70 to-slate-900/90"
        } ${perm.tapped ? "rotate-12" : ""}`}
      >
        <div className={`h-0.5 w-full ${isLand ? "bg-amber-500/60" : "bg-blue-500/40"}`} />
        <div className="flex flex-1 flex-col justify-between p-1">
          <span className={`line-clamp-2 text-[0.55rem] font-semibold leading-tight ${cardColorClass(perm.card)}`}>
            {perm.card.face.name}
          </span>
          {perm.enteredThisTurn && (
            <span className="text-[0.45rem] text-yellow-500">{t("bf.newThisTurn")}</span>
          )}
        </div>
      </button>

      {menuOpen && (
        <BfMenu
          actions={menuActions}
          onClose={() => setMenuOpen(false)}
          onAction={(a) => onAction(a, perm.card.id)}
        />
      )}
    </div>
  );
}

interface PlaytestBattlefieldProps {
  battlefield: PlaytestPermanent[];
  graveyard: PlaytestPermanent["card"][];
  exile: PlaytestPermanent["card"][];
  onTap: (id: number) => void;
  onUntap: (id: number) => void;
  onDestroy: (id: number) => void;
  onExile: (id: number) => void;
  onBounce: (id: number) => void;
}

export function PlaytestBattlefield({
  battlefield,
  graveyard,
  exile,
  onTap,
  onUntap,
  onDestroy,
  onExile,
  onBounce,
}: PlaytestBattlefieldProps) {
  const { t } = useTranslation("playtest");

  function handleAction(action: BattlefieldAction, id: number) {
    switch (action) {
      case "tap":     onTap(id);     break;
      case "untap":   onUntap(id);   break;
      case "destroy": onDestroy(id); break;
      case "exile":   onExile(id);   break;
      case "bounce":  onBounce(id);  break;
    }
  }

  // Separate lands and non-lands for display grouping.
  const lands = battlefield.filter((p) => isLandType(p.card.face.cardType?.coreTypes ?? []));
  const nonLands = battlefield.filter((p) => !isLandType(p.card.face.cardType?.coreTypes ?? []));

  return (
    <div className="space-y-2">
      {/* Non-land permanents */}
      <div className="min-h-12 rounded-xl border border-white/8 bg-black/10 p-2">
        <div className="mb-1.5 text-[0.55rem] font-semibold uppercase tracking-wider text-slate-600">
          {t("bf.spells")} ({nonLands.length})
        </div>
        {nonLands.length === 0 ? (
          <div className="py-2 text-center text-xs text-slate-600">{t("bf.emptySpells")}</div>
        ) : (
          <div className="flex flex-wrap gap-2">
            {nonLands.map((p) => (
              <BfCard key={p.card.id} perm={p} onAction={handleAction} />
            ))}
          </div>
        )}
      </div>

      {/* Lands */}
      <div className="min-h-12 rounded-xl border border-amber-900/30 bg-amber-950/10 p-2">
        <div className="mb-1.5 text-[0.55rem] font-semibold uppercase tracking-wider text-amber-700">
          {t("bf.lands")} ({lands.length})
        </div>
        {lands.length === 0 ? (
          <div className="py-2 text-center text-xs text-amber-900/70">{t("bf.emptyLands")}</div>
        ) : (
          <div className="flex flex-wrap gap-2">
            {lands.map((p) => (
              <BfCard key={p.card.id} perm={p} onAction={handleAction} />
            ))}
          </div>
        )}
      </div>

      {/* Graveyard / Exile counts */}
      <div className="flex gap-2 text-xs text-slate-500">
        <span>⚰️ {t("bf.graveyard", { count: graveyard.length })}</span>
        <span>·</span>
        <span>🔮 {t("bf.exile", { count: exile.length })}</span>
        <span>·</span>
        <span>📚 {t("bf.library")}</span>
      </div>
    </div>
  );
}
