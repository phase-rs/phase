import { useState } from "react";

import type { CoreType, DebugAction, ManaColor, PlayerId, Zone } from "../../adapter/types";
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

function CreateTokenForm({ onDispatch }: Props) {
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
        name: name || "Token",
        power,
        toughness,
        core_types: coreTypes,
        subtypes,
        colors,
        keywords,
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
      <SubmitButton onClick={handleSubmit}>Create Token</SubmitButton>
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
      <AccordionItem label="Create Token" expanded={expanded === "token"} onToggle={() => toggle("token")}>
        <CreateTokenForm onDispatch={onDispatch} />
      </AccordionItem>
    </div>
  );
}
