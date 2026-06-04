import { useState } from "react";

import type { DebugAction, ManaType, PlayerCounterKind, PlayerId } from "../../adapter/types";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId";
import { useUiStore } from "../../stores/uiStore";
import {
  AccordionItem,
  FieldRow,
  ManaTypeSelect,
  NumberInput,
  PlayerSelect,
  SelectInput,
  SubmitButton,
  useAccordion,
} from "./debugFields";

const PLAYER_COUNTER_KINDS: readonly PlayerCounterKind[] = [
  "Poison",
  "Experience",
  "Rad",
  "Ticket",
] as const;

interface Props {
  onDispatch: (action: DebugAction) => void;
}

function SetLifeForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [life, setLife] = useState(20);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Life">
        <NumberInput value={life} onChange={setLife} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "SetLife", data: { player_id: playerId, life } })}>
        Set Life
      </SubmitButton>
    </>
  );
}

function DrawCardsForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [count, setCount] = useState(1);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Count">
        <NumberInput value={count} onChange={setCount} min={1} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "DrawCards", data: { player_id: playerId, count } })}>
        Draw Cards
      </SubmitButton>
    </>
  );
}

function MillForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [count, setCount] = useState(1);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Count">
        <NumberInput value={count} onChange={setCount} min={1} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "Mill", data: { player_id: playerId, count } })}>
        Mill
      </SubmitButton>
    </>
  );
}

function RevealForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [count, setCount] = useState(1);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Count">
        <NumberInput value={count} onChange={setCount} min={1} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "Reveal", data: { player_id: playerId, count } })}>
        Reveal Top
      </SubmitButton>
    </>
  );
}

function ShuffleLibraryForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "ShuffleLibrary", data: { player_id: playerId } })}>
        Shuffle Library
      </SubmitButton>
    </>
  );
}

function ProliferateForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "Proliferate", data: { player_id: playerId } })}>
        Proliferate
      </SubmitButton>
    </>
  );
}

function AddManaForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [mana, setMana] = useState<ManaType[]>([]);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Mana">
        <ManaTypeSelect value={mana} onChange={setMana} />
      </FieldRow>
      <SubmitButton
        onClick={() => onDispatch({ type: "AddMana", data: { player_id: playerId, mana } })}
        disabled={mana.length === 0}
      >
        Add Mana
      </SubmitButton>
    </>
  );
}

function ModifyPlayerCountersForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [counterKind, setCounterKind] = useState<PlayerCounterKind>("Poison");
  const [delta, setDelta] = useState(1);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Counter">
        <SelectInput value={counterKind} onChange={setCounterKind} options={PLAYER_COUNTER_KINDS} />
      </FieldRow>
      <FieldRow label="Delta">
        <NumberInput value={delta} onChange={setDelta} />
      </FieldRow>
      <SubmitButton
        onClick={() =>
          onDispatch({
            type: "ModifyPlayerCounters",
            data: { player_id: playerId, counter_kind: counterKind, delta },
          })
        }
      >
        Modify Counters
      </SubmitButton>
    </>
  );
}

function ModifyEnergyForm({ onDispatch }: Props) {
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [delta, setDelta] = useState(1);

  return (
    <>
      <FieldRow label="Player">
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label="Delta">
        <NumberInput value={delta} onChange={setDelta} />
      </FieldRow>
      <SubmitButton
        onClick={() => onDispatch({ type: "ModifyEnergy", data: { player_id: playerId, delta } })}
      >
        Modify Energy
      </SubmitButton>
    </>
  );
}

// Opens the debug library browser for the local (perspective) player. The
// engine only exposes the viewer's OWN library names in sandbox debug
// (`visibility.rs`), so this is intentionally scoped to the perspective seat
// rather than offering a player picker that would render opponent backs.
function BrowseLibraryForm() {
  const openDebugLibraryViewer = useUiStore((s) => s.openDebugLibraryViewer);
  const perspectivePlayerId = usePerspectivePlayerId();

  return (
    <>
      <p className="mb-2 px-2 text-[10px] text-gray-500">
        Opens a modal of your library in randomized order. Click a card to move
        it to any zone, or use the quick Battlefield / Hand buttons.
      </p>
      <SubmitButton onClick={() => openDebugLibraryViewer(perspectivePlayerId)}>
        Browse Library
      </SubmitButton>
    </>
  );
}

export function DebugPlayerActions({ onDispatch }: Props) {
  const { expanded, toggle } = useAccordion();

  return (
    <div>
      <AccordionItem label="Set Life" expanded={expanded === "life"} onToggle={() => toggle("life")}>
        <SetLifeForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Draw Cards" expanded={expanded === "draw"} onToggle={() => toggle("draw")}>
        <DrawCardsForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Mill" expanded={expanded === "mill"} onToggle={() => toggle("mill")}>
        <MillForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Reveal Top" expanded={expanded === "reveal"} onToggle={() => toggle("reveal")}>
        <RevealForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Shuffle Library" expanded={expanded === "shuffle"} onToggle={() => toggle("shuffle")}>
        <ShuffleLibraryForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Browse Library" expanded={expanded === "browse"} onToggle={() => toggle("browse")}>
        <BrowseLibraryForm />
      </AccordionItem>
      <AccordionItem label="Proliferate" expanded={expanded === "proliferate"} onToggle={() => toggle("proliferate")}>
        <ProliferateForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Add Mana" expanded={expanded === "mana"} onToggle={() => toggle("mana")}>
        <AddManaForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Modify Counters" expanded={expanded === "counters"} onToggle={() => toggle("counters")}>
        <ModifyPlayerCountersForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label="Modify Energy" expanded={expanded === "energy"} onToggle={() => toggle("energy")}>
        <ModifyEnergyForm onDispatch={onDispatch} />
      </AccordionItem>
    </div>
  );
}
