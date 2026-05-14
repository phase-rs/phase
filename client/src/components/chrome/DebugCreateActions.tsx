import { useEffect, useMemo, useState } from "react";

import type { CoreType, DebugAction, ManaColor, PlayerId, Zone } from "../../adapter/types";
import {
  listTokenPresets,
  type TokenCategory,
  type TokenPreset,
} from "../../services/engineRuntime";
import {
  AccordionItem,
  CardNameAutocomplete,
  CheckboxInput,
  FieldRow,
  NumberInput,
  PlayerSelect,
  SelectInput,
  SubmitButton,
  TextInput,
  useAccordion,
} from "./debugFields";

const ZONES: readonly Zone[] = [
  "Battlefield",
  "Hand",
  "Graveyard",
  "Exile",
  "Library",
  "Stack",
  "Command",
] as const;

const CORE_TYPES: readonly CoreType[] = [
  "Creature",
  "Artifact",
  "Enchantment",
  "Land",
  "Planeswalker",
  "Instant",
  "Sorcery",
  "Battle",
  "Kindred",
  "Tribal",
  "Dungeon",
] as const;

const MANA_COLORS: readonly ManaColor[] = [
  "White",
  "Blue",
  "Black",
  "Red",
  "Green",
] as const;

const COLOR_LABELS: Record<ManaColor, string> = {
  White: "W",
  Blue: "U",
  Black: "B",
  Red: "R",
  Green: "G",
};

interface Props {
  onDispatch: (action: DebugAction) => void;
}

function CreateCardForm({ onDispatch }: Props) {
  const [cardName, setCardName] = useState("");
  const [owner, setOwner] = useState<PlayerId>(0);
  const [zone, setZone] = useState<Zone>("Hand");

  return (
    <>
      <FieldRow label="Card Name">
        <CardNameAutocomplete value={cardName} onChange={setCardName} placeholder="Lightning Bolt" />
      </FieldRow>
      <FieldRow label="Owner">
        <PlayerSelect value={owner} onChange={setOwner} />
      </FieldRow>
      <FieldRow label="Zone">
        <SelectInput value={zone} onChange={setZone} options={ZONES} />
      </FieldRow>
      <SubmitButton
        onClick={() =>
          onDispatch({ type: "CreateCard", data: { card_name: cardName, owner, zone } })
        }
        disabled={!cardName.trim()}
      >
        Create Card
      </SubmitButton>
    </>
  );
}

// Stable header text per `TokenCategory`. The engine ships category as
// pure data (variant tag); the FE maps it to display copy here. Sort key is
// used to order groups in the dropdown.
const CATEGORY_LABELS: { key: string; label: string; sort: number }[] = [
  { key: "PredefinedArtifact", label: "Artifact tokens (with abilities)", sort: 0 },
  { key: "Creature", label: "Creature tokens", sort: 1 },
  { key: "Aura", label: "Auras / Roles / Curses", sort: 2 },
  { key: "Equipment", label: "Equipment tokens", sort: 3 },
  { key: "Vehicle", label: "Vehicle tokens", sort: 4 },
  { key: "Enchantment", label: "Enchantment tokens", sort: 5 },
  { key: "Land", label: "Land tokens", sort: 6 },
  { key: "Artifact", label: "Other artifact tokens", sort: 7 },
];

function categoryKey(c: TokenCategory): string {
  return typeof c === "string" ? c : "PredefinedArtifact";
}

function categoryLabel(c: TokenCategory): string {
  if (typeof c !== "string") {
    return `${c.PredefinedArtifact.kind} tokens`;
  }
  return CATEGORY_LABELS.find((x) => x.key === c)?.label ?? c;
}

function presetSummary(p: TokenPreset): string {
  const ch = p.body;
  const pt =
    ch.power !== null && ch.toughness !== null ? `${ch.power}/${ch.toughness} ` : "";
  const colors = ch.colors.length === 0 ? "C" : ch.colors.map((c) => c[0]).join("");
  return `${pt}${colors} ${ch.display_name}`;
}

function CatalogTokenForm({ onDispatch }: Props) {
  const [owner, setOwner] = useState<PlayerId>(0);
  const [presets, setPresets] = useState<TokenPreset[] | null>(null);
  const [search, setSearch] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    listTokenPresets()
      .then((p) => setPresets(p))
      .catch((e: unknown) => {
        setLoadError(e instanceof Error ? e.message : String(e));
      });
  }, []);

  const filtered = useMemo(() => {
    if (!presets) return [];
    const q = search.trim().toLowerCase();
    if (!q) return presets;
    return presets.filter((p) => {
      if (p.body.display_name.toLowerCase().includes(q)) return true;
      if (p.body.subtypes.some((s) => s.toLowerCase().includes(q))) return true;
      return false;
    });
  }, [presets, search]);

  const grouped = useMemo(() => {
    const groups = new Map<string, TokenPreset[]>();
    for (const p of filtered) {
      const key = categoryKey(p.category);
      const arr = groups.get(key) ?? [];
      arr.push(p);
      groups.set(key, arr);
    }
    // Sort within each group by (power, toughness, name).
    for (const arr of groups.values()) {
      arr.sort((a, b) => {
        const ap = a.body.power ?? -1;
        const bp = b.body.power ?? -1;
        if (ap !== bp) return ap - bp;
        const at = a.body.toughness ?? -1;
        const bt = b.body.toughness ?? -1;
        if (at !== bt) return at - bt;
        return a.body.display_name.localeCompare(b.body.display_name);
      });
    }
    return groups;
  }, [filtered]);

  const orderedGroups = useMemo(() => {
    const keys = Array.from(grouped.keys());
    keys.sort((a, b) => {
      const ai = CATEGORY_LABELS.find((c) => c.key === a)?.sort ?? 99;
      const bi = CATEGORY_LABELS.find((c) => c.key === b)?.sort ?? 99;
      return ai - bi;
    });
    return keys;
  }, [grouped]);

  const handleSubmit = () => {
    const preset = presets?.find((p) => p.id === selectedId);
    if (!preset) return;
    onDispatch({
      type: "CreateToken",
      data: {
        owner,
        characteristics: preset.body,
      },
    });
  };

  if (loadError) {
    return (
      <div className="px-2 py-3 text-xs text-red-400">
        Failed to load token catalog: {loadError}
      </div>
    );
  }
  if (!presets) {
    return <div className="px-2 py-3 text-xs text-gray-500">Loading token catalog…</div>;
  }

  return (
    <>
      <FieldRow label="Owner">
        <PlayerSelect value={owner} onChange={setOwner} />
      </FieldRow>
      <FieldRow label="Search">
        <TextInput value={search} onChange={setSearch} placeholder="Name or subtype" />
      </FieldRow>
      <div className="mb-2 max-h-64 overflow-y-auto rounded border border-gray-800 bg-gray-950/40 p-1">
        {orderedGroups.length === 0 && (
          <div className="px-2 py-2 text-xs text-gray-500">No presets match.</div>
        )}
        {orderedGroups.map((key) => {
          const items = grouped.get(key) ?? [];
          const sample = items[0]?.category;
          return (
            <div key={key} className="mb-2">
              <div className="px-1 pb-1 font-mono text-[10px] uppercase tracking-wider text-gray-500">
                {sample !== undefined ? categoryLabel(sample) : key}
              </div>
              {items.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => setSelectedId(p.id)}
                  className={
                    "block w-full rounded px-2 py-1 text-left font-mono text-[11px] transition-colors " +
                    (selectedId === p.id
                      ? "bg-blue-500/20 text-blue-200"
                      : "text-gray-300 hover:bg-gray-800/60")
                  }
                >
                  <span>{presetSummary(p)}</span>
                  {p.fidelity === "PartialMissingAbilities" && (
                    <span className="ml-1 rounded border border-amber-500/40 bg-amber-500/10 px-1 text-[9px] text-amber-300">
                      body only
                    </span>
                  )}
                </button>
              ))}
            </div>
          );
        })}
      </div>
      <SubmitButton onClick={handleSubmit} disabled={!selectedId}>
        Create Selected Token
      </SubmitButton>
    </>
  );
}

function CustomTokenForm({ onDispatch }: Props) {
  const [name, setName] = useState("");
  const [owner, setOwner] = useState<PlayerId>(0);
  const [power, setPower] = useState(1);
  const [toughness, setToughness] = useState(1);
  const [coreTypes, setCoreTypes] = useState<CoreType[]>(["Creature"]);
  const [subtypesText, setSubtypesText] = useState("");
  const [colors, setColors] = useState<ManaColor[]>([]);
  const [keywordsText, setKeywordsText] = useState("");

  const toggleCoreType = (ct: CoreType) => {
    setCoreTypes((prev) =>
      prev.includes(ct) ? prev.filter((t) => t !== ct) : [...prev, ct],
    );
  };

  const toggleColor = (c: ManaColor) => {
    setColors((prev) =>
      prev.includes(c) ? prev.filter((x) => x !== c) : [...prev, c],
    );
  };

  const handleSubmit = () => {
    const subtypes = subtypesText
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const keywords = keywordsText
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);

    onDispatch({
      type: "CreateToken",
      data: {
        owner,
        characteristics: {
          display_name: name || "Token",
          power,
          toughness,
          core_types: coreTypes,
          subtypes,
          supertypes: [],
          colors,
          keywords,
        },
      },
    });
  };

  return (
    <>
      <FieldRow label="Name">
        <CardNameAutocomplete value={name} onChange={setName} placeholder="Token" />
      </FieldRow>
      <FieldRow label="Owner">
        <PlayerSelect value={owner} onChange={setOwner} />
      </FieldRow>
      <FieldRow label="Power">
        <NumberInput value={power} onChange={setPower} />
      </FieldRow>
      <FieldRow label="Toughness">
        <NumberInput value={toughness} onChange={setToughness} />
      </FieldRow>
      <FieldRow label="Types">
        <div className="flex flex-wrap gap-1">
          {CORE_TYPES.map((ct) => (
            <CheckboxInput
              key={ct}
              checked={coreTypes.includes(ct)}
              onChange={() => toggleCoreType(ct)}
              label={ct}
            />
          ))}
        </div>
      </FieldRow>
      <FieldRow label="Subtypes">
        <TextInput value={subtypesText} onChange={setSubtypesText} placeholder="Human, Soldier" />
      </FieldRow>
      <FieldRow label="Colors">
        <div className="flex flex-wrap gap-1">
          {MANA_COLORS.map((c) => (
            <button
              key={c}
              type="button"
              onClick={() => toggleColor(c)}
              className={
                "rounded-full border px-2 py-0.5 font-mono text-[10px] transition-colors " +
                (colors.includes(c)
                  ? "border-blue-500/60 bg-blue-500/20 text-blue-300"
                  : "border-gray-700 bg-transparent text-gray-600 hover:border-gray-600")
              }
            >
              {COLOR_LABELS[c]}
            </button>
          ))}
        </div>
      </FieldRow>
      <FieldRow label="Keywords">
        <TextInput value={keywordsText} onChange={setKeywordsText} placeholder="Flying, Haste" />
      </FieldRow>
      <SubmitButton onClick={handleSubmit}>Create Custom Token</SubmitButton>
    </>
  );
}

export function DebugCreateActions({ onDispatch }: Props) {
  const { expanded, toggle } = useAccordion();

  return (
    <div>
      <AccordionItem label="Create Card" expanded={expanded === "card"} onToggle={() => toggle("card")}>
        <CreateCardForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Create Token (Catalog)" expanded={expanded === "token-catalog"} onToggle={() => toggle("token-catalog")}>
        <CatalogTokenForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Create Token (Custom)" expanded={expanded === "token-custom"} onToggle={() => toggle("token-custom")}>
        <CustomTokenForm onDispatch={onDispatch} />
      </AccordionItem>
    </div>
  );
}
