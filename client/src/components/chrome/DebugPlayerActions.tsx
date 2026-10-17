import { useTranslation } from "react-i18next";
import { useState } from "react";

import type { DebugAction, ManaType, PlayerCounterKind, PlayerId } from "../../adapter/types";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId";
import { useUiStore } from "../../stores/uiStore";
import {
  AccordionItem,
  CheckboxInput,
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
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [life, setLife] = useState(20);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.life")}>
        <NumberInput value={life} onChange={setLife} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "SetLife", data: { player_id: playerId, life } })}>
        {t("sandboxTools.setLife")}
      </SubmitButton>
    </>
  );
}

function DrawCardsForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [count, setCount] = useState(1);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.count")}>
        <NumberInput value={count} onChange={setCount} min={1} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "DrawCards", data: { player_id: playerId, count } })}>
        {t("sandboxTools.drawCards")}
      </SubmitButton>
    </>
  );
}

function MillForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [count, setCount] = useState(1);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.count")}>
        <NumberInput value={count} onChange={setCount} min={1} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "Mill", data: { player_id: playerId, count } })}>
        {t("sandboxTools.mill")}
      </SubmitButton>
    </>
  );
}

function RevealForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [count, setCount] = useState(1);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.count")}>
        <NumberInput value={count} onChange={setCount} min={1} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "Reveal", data: { player_id: playerId, count } })}>
        {t("sandboxTools.revealTop")}
      </SubmitButton>
    </>
  );
}

function ShuffleLibraryForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "ShuffleLibrary", data: { player_id: playerId } })}>
        {t("sandboxTools.shuffleLibrary")}
      </SubmitButton>
    </>
  );
}

function ProliferateForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "Proliferate", data: { player_id: playerId } })}>
        {t("sandboxTools.proliferate")}
      </SubmitButton>
    </>
  );
}

function AddManaForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [mana, setMana] = useState<ManaType[]>([]);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.mana")}>
        <ManaTypeSelect value={mana} onChange={setMana} />
      </FieldRow>
      <SubmitButton
        onClick={() => onDispatch({ type: "AddMana", data: { player_id: playerId, mana } })}
        disabled={mana.length === 0}
      >
        {t("sandboxTools.addMana")}
      </SubmitButton>
    </>
  );
}

function InfiniteManaForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [enabled, setEnabled] = useState(true);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.state")}>
        <CheckboxInput checked={enabled} onChange={setEnabled} label={t("sandboxTools.enabled")} />
      </FieldRow>
      <SubmitButton onClick={() => onDispatch({ type: "SetInfiniteMana", data: { player_id: playerId, enabled } })}>
        {t("sandboxTools.apply")}
      </SubmitButton>
    </>
  );
}

function ModifyPlayerCountersForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [counterKind, setCounterKind] = useState<PlayerCounterKind>("Poison");
  const [delta, setDelta] = useState(1);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.counter")}>
        <SelectInput value={counterKind} onChange={setCounterKind} options={PLAYER_COUNTER_KINDS} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.delta")}>
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
        {t("sandboxTools.modifyCounters")}
      </SubmitButton>
    </>
  );
}

function ModifyEnergyForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [playerId, setPlayerId] = useState<PlayerId>(0);
  const [delta, setDelta] = useState(1);

  return (
    <>
      <FieldRow label={t("sandboxTools.player")}>
        <PlayerSelect value={playerId} onChange={setPlayerId} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.delta")}>
        <NumberInput value={delta} onChange={setDelta} />
      </FieldRow>
      <SubmitButton
        onClick={() => onDispatch({ type: "ModifyEnergy", data: { player_id: playerId, delta } })}
      >
        {t("sandboxTools.modifyEnergy")}
      </SubmitButton>
    </>
  );
}

// Opens the debug library browser for the local (perspective) player. The
// engine supplies its separately authorized OWN-library debug projection, so
// this remains scoped to the perspective seat rather than offering a picker
// that could expose opponent library identities.
function BrowseLibraryForm() {
  const { t } = useTranslation("game");
  const openDebugLibraryViewer = useUiStore((s) => s.openDebugLibraryViewer);
  const perspectivePlayerId = usePerspectivePlayerId();

  const openLibrary = () => {
    // DebugPanel sits above ordinary modal layers. Close it before mounting the
    // aria-modal library viewer, just like the panel's Report Card action, so
    // no visually higher background surface remains pointer-accessible.
    if (useUiStore.getState().debugPanelOpen) {
      useUiStore.getState().toggleDebugPanel();
    }
    openDebugLibraryViewer(perspectivePlayerId);
  };

  return (
    <>
      <p className="mb-2 px-2 text-[10px] text-gray-500">
        {t("sandboxTools.browseDescription")}
      </p>
      <SubmitButton onClick={openLibrary}>
        {t("sandboxTools.browseLibrary")}
      </SubmitButton>
    </>
  );
}

export function DebugPlayerActions({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const { expanded, toggle } = useAccordion();

  return (
    <div>
      <AccordionItem label={t("sandboxTools.setLife")} expanded={expanded === "life"} onToggle={() => toggle("life")}>
        <SetLifeForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.drawCards")} expanded={expanded === "draw"} onToggle={() => toggle("draw")}>
        <DrawCardsForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.mill")} expanded={expanded === "mill"} onToggle={() => toggle("mill")}>
        <MillForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.revealTop")} expanded={expanded === "reveal"} onToggle={() => toggle("reveal")}>
        <RevealForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.shuffleLibrary")} expanded={expanded === "shuffle"} onToggle={() => toggle("shuffle")}>
        <ShuffleLibraryForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.browseLibrary")} expanded={expanded === "browse"} onToggle={() => toggle("browse")}>
        <BrowseLibraryForm />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.proliferate")} expanded={expanded === "proliferate"} onToggle={() => toggle("proliferate")}>
        <ProliferateForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.addMana")} expanded={expanded === "mana"} onToggle={() => toggle("mana")}>
        <AddManaForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem
        label={t("sandboxTools.infiniteMana")}
        expanded={expanded === "infinite-mana"}
        onToggle={() => toggle("infinite-mana")}
      >
        <InfiniteManaForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.modifyCounters")} expanded={expanded === "counters"} onToggle={() => toggle("counters")}>
        <ModifyPlayerCountersForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.modifyEnergy")} expanded={expanded === "energy"} onToggle={() => toggle("energy")}>
        <ModifyEnergyForm onDispatch={onDispatch} />
      </AccordionItem>
    </div>
  );
}
