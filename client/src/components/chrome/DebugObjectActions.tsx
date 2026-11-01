import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  AttachTarget,
  CounterType,
  DebugAction,
  GameObject,
  ObjectId,
  PlayerId,
  Zone,
} from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import {
  AccordionItem,
  CheckboxInput,
  deriveAttachmentInfo,
  FieldRow,
  NumberInput,
  ObjectSelect,
  PlayerSelect,
  SelectInput,
  SubmitButton,
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

const LIBRARY_POSITIONS = ["Top", "Bottom"] as const;
type LibraryEndPosition = (typeof LIBRARY_POSITIONS)[number];

const COUNTER_TYPES: readonly CounterType[] = [
  "P1P1",
  "M1M1",
  "loyalty",
  "lore",
  "charge",
  "stun",
  "time",
  "fate",
  "quest",
  "verse",
] as const;

// ── Filter presets ──────────────────────────────────────────────────────
// Each debug action declares its actual constraint via an ObjectSelect filter.
// Centralizing these here makes the "what can this action operate on" question
// reviewable at a glance, and keeps the FE matching what the engine handlers
// actually accept (e.g., SetTapped is a no-op for off-battlefield objects).

const onBattlefield = (obj: GameObject) => obj.zone === "Battlefield";
const isAttachable = (obj: GameObject) =>
  obj.zone === "Battlefield" &&
  obj.card_types.subtypes.some(
    (s) => s === "Aura" || s === "Equipment" || s === "Fortification",
  );

interface Props {
  onDispatch: (action: DebugAction) => void;
}

function MoveToZoneForm({ onDispatch }: Props) {
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [toZone, setToZone] = useState<Zone>("Battlefield");
  const [libraryPosition, setLibraryPosition] = useState<LibraryEndPosition>("Bottom");
  const { t } = useTranslation("game");
  const [simulate, setSimulate] = useState(false);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} />
      <FieldRow label={t("sandboxTools.toZone")}>
        <SelectInput value={toZone} onChange={setToZone} options={ZONES} />
      </FieldRow>
      {toZone === "Library" && (
        <FieldRow label={t("sandboxTools.position")}>
          <SelectInput
            value={libraryPosition}
            onChange={setLibraryPosition}
            options={LIBRARY_POSITIONS}
          />
        </FieldRow>
      )}
      <CheckboxInput
        checked={simulate}
        onChange={setSimulate}
        label={t("debugMove.simulate")}
      />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({
            type: "MoveToZone",
            data: {
              object_id: objectId,
              to_zone: toZone,
              ...(toZone === "Library"
                ? { library_position: { type: libraryPosition } }
                : {}),
              simulate,
            },
          })
        }
      >
        {t("sandboxTools.move")}
      </SubmitButton>
    </>
  );
}

function RemoveObjectForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({ type: "RemoveObject", data: { object_id: objectId } })
        }
      >
        {t("sandboxTools.remove")}
      </SubmitButton>
    </>
  );
}

function CreateTokenCopyForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [sourceId, setSourceId] = useState<ObjectId | null>(null);
  const [owner, setOwner] = useState<PlayerId>(0);
  const [nonlegendary, setNonlegendary] = useState(false);
  const [count, setCount] = useState(1);

  return (
    <>
      <ObjectSelect
        value={sourceId}
        onChange={setSourceId}
        label={t("sandboxTools.source")}
        placeholder={t("sandboxTools.pickCopySource")}
      />
      <FieldRow label={t("sandboxTools.owner")}>
        <PlayerSelect value={owner} onChange={setOwner} />
      </FieldRow>
      <FieldRow label={t("debugCreate.copies")}>
        <NumberInput value={count} onChange={setCount} min={0} />
      </FieldRow>
      <FieldRow label="">
        <CheckboxInput
          checked={nonlegendary}
          onChange={setNonlegendary}
          label={t("sandboxTools.nonlegendary")}
        />
      </FieldRow>
      <SubmitButton
        disabled={sourceId == null}
        onClick={() =>
          sourceId != null &&
          onDispatch({
            type: "CreateTokenCopy",
            data: { source_id: sourceId, owner, nonlegendary, count },
          })
        }
      >
        {t("sandboxTools.copyObject")}
      </SubmitButton>
    </>
  );
}

function SetBasePTForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [power, setPower] = useState(0);
  const [toughness, setToughness] = useState(0);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <FieldRow label={t("sandboxTools.power")}>
        <NumberInput value={power} onChange={setPower} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.toughness")}>
        <NumberInput value={toughness} onChange={setToughness} />
      </FieldRow>
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({
            type: "SetBasePowerToughness",
            data: { object_id: objectId, power, toughness },
          })
        }
      >
        {t("sandboxTools.setPT")}
      </SubmitButton>
    </>
  );
}

function ModifyCountersForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [counterType, setCounterType] = useState<CounterType>("P1P1");
  const [delta, setDelta] = useState(1);
  const object = useGameStore((s) =>
    objectId == null ? undefined : s.gameState?.objects[objectId],
  );
  const counterTypes = useMemo<CounterType[]>(
    () => Array.from(new Set([...COUNTER_TYPES, ...Object.keys(object?.counters ?? {})])),
    [object?.counters],
  );

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <FieldRow label={t("sandboxTools.counter")}>
        <SelectInput
          value={counterType}
          onChange={setCounterType}
          options={counterTypes}
        />
      </FieldRow>
      <FieldRow label={t("sandboxTools.delta")}>
        <NumberInput value={delta} onChange={setDelta} />
      </FieldRow>
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({
            type: "ModifyCounters",
            data: { object_id: objectId, counter_type: counterType, delta },
          })
        }
      >
        {t("sandboxTools.modifyCounters")}
      </SubmitButton>
    </>
  );
}

function SetTappedForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [tapped, setTapped] = useState(true);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <CheckboxInput checked={tapped} onChange={setTapped} label={t("sandboxTools.tapped")} />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({ type: "SetTapped", data: { object_id: objectId, tapped } })
        }
      >
        {t("sandboxTools.setTapState")}
      </SubmitButton>
    </>
  );
}

function SetPreparedForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [prepared, setPrepared] = useState(true);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <CheckboxInput checked={prepared} onChange={setPrepared} label={t("sandboxTools.prepared")} />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({ type: "SetPrepared", data: { object_id: objectId, prepared } })
        }
      >
        {t("sandboxTools.setPreparedState")}
      </SubmitButton>
    </>
  );
}

function SetControllerForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [controller, setController] = useState<PlayerId>(0);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <FieldRow label={t("sandboxTools.controller")}>
        <PlayerSelect value={controller} onChange={setController} />
      </FieldRow>
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({
            type: "SetController",
            data: { object_id: objectId, controller },
          })
        }
      >
        {t("sandboxTools.setController")}
      </SubmitButton>
    </>
  );
}

function SetSummoningSicknessForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [sick, setSick] = useState(false);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <CheckboxInput checked={sick} onChange={setSick} label={t("sandboxTools.summoningSick")} />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({ type: "SetSummoningSickness", data: { object_id: objectId, sick } })
        }
      >
        {t("sandboxTools.setSummoningSickness")}
      </SubmitButton>
    </>
  );
}

function SetFaceStateForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);
  const [faceDown, setFaceDown] = useState(false);
  const [transformed, setTransformed] = useState(false);
  const [flipped, setFlipped] = useState(false);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} />
      <CheckboxInput checked={faceDown} onChange={setFaceDown} label={t("sandboxTools.faceDown")} />
      <CheckboxInput checked={transformed} onChange={setTransformed} label={t("sandboxTools.transformed")} />
      <CheckboxInput checked={flipped} onChange={setFlipped} label={t("sandboxTools.flipped")} />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null &&
          onDispatch({
            type: "SetFaceState",
            data: { object_id: objectId, face_down: faceDown, transformed, flipped },
          })
        }
      >
        {t("sandboxTools.setFaceState")}
      </SubmitButton>
    </>
  );
}

function AttachForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [sourceId, setSourceId] = useState<ObjectId | null>(null);
  const [targetKind, setTargetKind] = useState<"Object" | "Player">("Object");
  const [targetObjectId, setTargetObjectId] = useState<ObjectId | null>(null);
  const [targetPlayerId, setTargetPlayerId] = useState<PlayerId>(0);

  const sourceObj = useGameStore((s) =>
    sourceId != null ? ((s.gameState?.objects[sourceId] as GameObject | undefined) ?? null) : null,
  );
  const info = useMemo(
    () =>
      deriveAttachmentInfo({
        keywords: sourceObj?.keywords ?? null,
        subtypes: sourceObj?.card_types?.subtypes ?? null,
      }),
    [sourceObj],
  );

  // Auto-flip kind when the source dictates only one possibility.
  useEffect(() => {
    if (info.canTargetPlayer && !info.canTargetObject) setTargetKind("Player");
    else if (!info.canTargetPlayer && info.canTargetObject) setTargetKind("Object");
  }, [info.canTargetPlayer, info.canTargetObject]);

  const buildTarget = (): AttachTarget | null => {
    if (targetKind === "Player") return { type: "Player", data: targetPlayerId };
    if (targetObjectId == null) return null;
    return { type: "Object", data: targetObjectId };
  };

  const target = buildTarget();
  const canSubmit = sourceId != null && target != null;

  return (
    <>
      <ObjectSelect
        value={sourceId}
        onChange={setSourceId}
        filter={isAttachable}
        label={t("sandboxTools.attach")}
        placeholder={t("sandboxTools.pickAttachment")}
      />
      {info.canTargetPlayer && info.canTargetObject && (
        <FieldRow label={t("sandboxTools.targetKind")}>
          <SelectInput
            value={targetKind}
            onChange={setTargetKind}
            options={["Object", "Player"] as const}
          />
        </FieldRow>
      )}
      {targetKind === "Object" && (
        <ObjectSelect
          value={targetObjectId}
          onChange={setTargetObjectId}
          filter={info.objectFilter}
          label={t("sandboxTools.toObject")}
          placeholder={t("sandboxTools.pickHost")}
        />
      )}
      {targetKind === "Player" && (
        <FieldRow label={t("sandboxTools.toPlayer")}>
          <PlayerSelect value={targetPlayerId} onChange={setTargetPlayerId} />
        </FieldRow>
      )}
      <SubmitButton
        disabled={!canSubmit}
        onClick={() => {
          if (sourceId != null && target) {
            onDispatch({ type: "Attach", data: { object_id: sourceId, target } });
          }
        }}
      >
        {t("sandboxTools.attach")}
      </SubmitButton>
    </>
  );
}

function DetachForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [objectId, setObjectId] = useState<ObjectId | null>(null);

  return (
    <>
      <ObjectSelect value={objectId} onChange={setObjectId} filter={onBattlefield} />
      <SubmitButton
        disabled={objectId == null}
        onClick={() =>
          objectId != null && onDispatch({ type: "Detach", data: { object_id: objectId } })
        }
      >
        {t("sandboxTools.detach")}
      </SubmitButton>
    </>
  );
}

export function DebugObjectActions({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const { expanded, toggle } = useAccordion();

  return (
    <div>
      <AccordionItem label={t("sandboxTools.moveToZone")} expanded={expanded === "move"} onToggle={() => toggle("move")}>
        <MoveToZoneForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.removeObject")} expanded={expanded === "remove"} onToggle={() => toggle("remove")}>
        <RemoveObjectForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.copyObject")} expanded={expanded === "copy"} onToggle={() => toggle("copy")}>
        <CreateTokenCopyForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.setBasePT")} expanded={expanded === "pt"} onToggle={() => toggle("pt")}>
        <SetBasePTForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.modifyCounters")} expanded={expanded === "counters"} onToggle={() => toggle("counters")}>
        <ModifyCountersForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.setTapped")} expanded={expanded === "tapped"} onToggle={() => toggle("tapped")}>
        <SetTappedForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.setPrepared")} expanded={expanded === "prepared"} onToggle={() => toggle("prepared")}>
        <SetPreparedForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.setController")} expanded={expanded === "controller"} onToggle={() => toggle("controller")}>
        <SetControllerForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.summoningSickness")} expanded={expanded === "sick"} onToggle={() => toggle("sick")}>
        <SetSummoningSicknessForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.faceState")} expanded={expanded === "face"} onToggle={() => toggle("face")}>
        <SetFaceStateForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.attach")} expanded={expanded === "attach"} onToggle={() => toggle("attach")}>
        <AttachForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.detach")} expanded={expanded === "detach"} onToggle={() => toggle("detach")}>
        <DetachForm onDispatch={onDispatch} />
      </AccordionItem>
    </div>
  );
}
