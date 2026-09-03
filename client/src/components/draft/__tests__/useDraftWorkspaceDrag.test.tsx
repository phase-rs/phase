import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { StrictMode, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DraftPickInteractionSnapshot,
  DraftDropDispatch,
  DraftDropRequest,
  WorkspaceDragSource,
} from "../workspace/useDraftWorkspaceDrag";
import { useDraftWorkspaceDrag } from "../workspace/useDraftWorkspaceDrag";
import type { PackDropAdmission, PackDropSettlement, PackDropSource } from "../PackDisplay";

const card = {
  instance_id: "card-1", name: "Card One", set_code: "TST", collector_number: "1",
  rarity: "common", colors: [], cmc: 1, type_line: "Card",
};

function rect(left: number, top: number, right: number, bottom: number): DOMRect {
  return { left, top, right, bottom, width: right - left, height: bottom - top, x: left, y: top, toJSON: () => ({}) } as DOMRect;
}

function firePointerActivation(
  element: Element,
  type: "click" | "dblclick",
  { detail, pointerId, pointerType }: { detail: number; pointerId: number; pointerType: string },
) {
  fireEvent(element, new PointerEvent(type, { bubbles: true, detail, pointerId, pointerType }));
}

function createInteraction() {
  let snapshot: DraftPickInteractionSnapshot = { interactionGeneration: 1, pickInteractionLocked: false, pendingPickIntent: null };
  const listeners = new Set<() => void>();
  return {
    read: () => snapshot,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => listeners.delete(listener); },
    publish(next: DraftPickInteractionSnapshot) { snapshot = next; for (const listener of listeners) listener(); },
    listenerCount: () => listeners.size,
  };
}

function Harness({ interaction, onDrop, onAdmission = vi.fn(), onSettled, expanded = false, enabled = true, targetVersion = 0, sourceOverride, workspaceSourceOverride, workspaceTouchEnabled = false, secondaryActivation }: {
  interaction: ReturnType<typeof createInteraction>;
  onDrop(request: DraftDropRequest): DraftDropDispatch;
  onAdmission?: (admission: PackDropAdmission) => void;
  onSettled: (result: PackDropSettlement) => void;
  expanded?: boolean;
  enabled?: boolean;
  targetVersion?: number;
  sourceOverride?: PackDropSource;
  workspaceSourceOverride?: WorkspaceDragSource;
  workspaceTouchEnabled?: boolean;
  secondaryActivation?: { readonly surface: "pack" | "workspace"; readonly sourceInstanceId: string };
}) {
  const [clicks, setClicks] = useState(0);
  const [doubleClicks, setDoubleClicks] = useState(0);
  const drag = useDraftWorkspaceDrag({
    enabled,
    readPickInteraction: interaction.read,
    subscribePickInteraction: interaction.subscribe,
    onDrop: onDrop as never,
    resolveCollapsedSideboardColumn: () => 2,
  });
  const source: PackDropSource = sourceOverride ?? {
    kind: "pick" as const,
    authorityId: "card-1",
    sourceInstanceId: "card-1",
    instanceIds: ["card-1"] as const,
    cards: [card],
    sourceIndices: [0],
    interactionGeneration: 1,
    previewWidth: 146,
    previewHeight: 204,
    onAdmission,
    onSettled,
  };
  return (
    <div>
      <div
        data-testid="source"
        onPointerDown={(event) => workspaceSourceOverride
          ? drag.handleWorkspacePointerDown(event, workspaceSourceOverride, workspaceTouchEnabled)
          : drag.handlePointerDown(event, source)}
        onPointerMove={drag.handlePointerMove}
        onPointerUp={drag.handlePointerUp}
        onPointerCancel={drag.handlePointerCancel}
        onLostPointerCapture={drag.handleLostPointerCapture}
        onClick={(event) => {
          const pointerEvent = event.nativeEvent as PointerEvent;
          if (!drag.consumeCompatibilityActivation({
            kind: "click", detail: event.detail, pointerId: pointerEvent.pointerId ?? null,
            pointerType: pointerEvent.pointerType, surface: workspaceSourceOverride ? "workspace" : "pack",
            sourceInstanceId: workspaceSourceOverride?.instanceIds[0] ?? source.sourceInstanceId,
          })) setClicks((count) => count + 1);
        }}
        onDoubleClick={(event) => {
          const pointerEvent = event.nativeEvent as PointerEvent;
          if (!drag.consumeCompatibilityActivation({
            kind: "double-click", detail: event.detail, pointerId: pointerEvent.pointerId ?? null,
            pointerType: pointerEvent.pointerType, surface: workspaceSourceOverride ? "workspace" : "pack",
            sourceInstanceId: workspaceSourceOverride?.instanceIds[0] ?? source.sourceInstanceId,
          })) setDoubleClicks((count) => count + 1);
        }}
      />
      {secondaryActivation !== undefined && (
        <button
          type="button"
          data-testid="secondary-activation"
          onClick={(event) => {
            const pointerEvent = event.nativeEvent as PointerEvent;
            if (!drag.consumeCompatibilityActivation({
              kind: "click", detail: event.detail, pointerId: pointerEvent.pointerId ?? null,
              pointerType: pointerEvent.pointerType, ...secondaryActivation,
            })) setClicks((count) => count + 1);
          }}
        />
      )}
      {expanded ? (
        <>
          <div data-testid="deck-board" ref={drag.registerBoard("deck")} />
          <div data-testid="deck-column-0" ref={drag.registerColumn("deck", 0)}>
            <div data-testid="deck-column-0-row-0" data-board-row="0" />
            <div data-testid="deck-column-0-row-1" data-board-row="1" />
          </div>
          <div data-testid="deck-column-1" ref={drag.registerColumn("deck", 1)} />
          <div data-testid="sideboard-board" ref={drag.registerBoard("sideboard")} />
          <div data-testid="sideboard-column-0" ref={drag.registerColumn("sideboard", 0)} />
        </>
      ) : <div key={targetVersion} data-testid="target" ref={drag.registerCollapsedSideboard} />}
      <button type="button" onClick={drag.dispose}>dispose</button>
      <output data-testid="clicks">{clicks}:{doubleClicks}</output>
      <output data-testid="announcement">{drag.announcement}</output>
      <output data-testid="deck-drop-state">{JSON.stringify(drag.dropState("deck"))}</output>
      <output data-testid="preview">{drag.dragPreview === null ? "" : JSON.stringify({
        ids: drag.dragPreview.source.instanceIds,
        cards: drag.dragPreview.source.cards.map((entry) => entry.instance_id),
        x: drag.dragPreview.clientX,
        y: drag.dragPreview.clientY,
      })}</output>
    </div>
  );
}

afterEach(cleanup);

describe("useDraftWorkspaceDrag", () => {
  it("moves_a_workspace_card_to_the_exact_column_without_dispatching_a_pick", () => {
    const interaction = createInteraction();
    const onDrop = vi.fn();
    const onWorkspaceDrop = vi.fn(() => true);
    const workspaceSource: WorkspaceDragSource = {
      kind: "workspace",
      instanceIds: ["card-1"],
      cards: [card],
      previewWidth: 146,
      previewHeight: 204,
      onDrop: onWorkspaceDrop,
    };
    render(
      <Harness
        interaction={interaction}
        onDrop={onDrop as never}
        onSettled={vi.fn()}
        workspaceSourceOverride={workspaceSource}
        expanded
      />,
    );
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    screen.getByTestId("deck-board").getBoundingClientRect = () => rect(0, 0, 200, 200);
    screen.getByTestId("deck-column-0").getBoundingClientRect = () => rect(0, 0, 100, 200);
    screen.getByTestId("deck-column-1").getBoundingClientRect = () => rect(100, 0, 200, 200);
    screen.getByTestId("deck-column-0-row-0").getBoundingClientRect = () => rect(0, 0, 100, 96);
    screen.getByTestId("deck-column-0-row-1").getBoundingClientRect = () => rect(0, 104, 100, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 50, clientY: 150, pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 50, clientY: 150, pointerId: 1, pointerType: "mouse" });

    expect(onWorkspaceDrop).toHaveBeenCalledWith({ zone: "deck", column: 0, row: 1 });
    expect(onDrop).not.toHaveBeenCalled();
    expect(screen.getByTestId("announcement")).toHaveTextContent("Moved Card One.");
  });

  it("moves_opted_in_touch_workspace_cards_across_zones_and_suppresses_the_drag_click", () => {
    const interaction = createInteraction();
    const onDrop = vi.fn();
    const onWorkspaceDrop = vi.fn(() => true);
    render(
      <Harness
        interaction={interaction}
        onDrop={onDrop as never}
        onSettled={vi.fn()}
        workspaceTouchEnabled
        workspaceSourceOverride={{
          kind: "workspace",
          instanceIds: ["card-1"],
          cards: [card],
          previewWidth: 146,
          previewHeight: 204,
          onDrop: onWorkspaceDrop,
        }}
        expanded
      />,
    );
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    screen.getByTestId("deck-board").getBoundingClientRect = () => rect(0, 0, 200, 200);
    screen.getByTestId("deck-column-0").getBoundingClientRect = () => rect(0, 0, 100, 200);
    screen.getByTestId("sideboard-board").getBoundingClientRect = () => rect(220, 0, 420, 200);
    screen.getByTestId("sideboard-column-0").getBoundingClientRect = () => rect(220, 0, 320, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 91, pointerType: "touch" });
    expect(source.setPointerCapture).toHaveBeenCalledWith(91);
    fireEvent.pointerMove(source, { clientX: 270, clientY: 80, pointerId: 91, pointerType: "touch" });
    fireEvent.pointerUp(source, { clientX: 270, clientY: 80, pointerId: 91, pointerType: "touch" });
    firePointerActivation(source, "click", { detail: 1, pointerId: 91, pointerType: "touch" });

    expect(onWorkspaceDrop).toHaveBeenCalledWith({ zone: "sideboard", column: 0 });
    expect(onDrop).not.toHaveBeenCalled();
    expect(screen.getByTestId("clicks")).toHaveTextContent("0:0");

    fireEvent.pointerDown(source, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 92, pointerType: "touch" });
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 92, pointerType: "touch" });
    fireEvent.click(source, { detail: 1, pointerType: "touch" });

    expect(onWorkspaceDrop).toHaveBeenCalledOnce();
    expect(screen.getByTestId("clicks")).toHaveTextContent("1:0");
  });

  it("cancels_a_workspace_drop_when_the_pick_lock_changes_mid_drag", () => {
    const interaction = createInteraction();
    const onWorkspaceDrop = vi.fn(() => true);
    render(
      <Harness
        interaction={interaction}
        onDrop={vi.fn() as never}
        onSettled={vi.fn()}
        workspaceSourceOverride={{
          kind: "workspace",
          instanceIds: ["card-1"],
          cards: [card],
          previewWidth: 146,
          previewHeight: 204,
          onDrop: onWorkspaceDrop,
        }}
        expanded
      />,
    );
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    screen.getByTestId("deck-board").getBoundingClientRect = () => rect(0, 0, 200, 200);
    screen.getByTestId("deck-column-0").getBoundingClientRect = () => rect(0, 0, 100, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 2, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 50, clientY: 50, pointerId: 2, pointerType: "mouse" });
    act(() => interaction.publish({
      interactionGeneration: 1,
      pickInteractionLocked: true,
      pendingPickIntent: { kind: "pick", instanceIds: ["other"], destination: "deck" },
    }));
    fireEvent.pointerUp(source, { clientX: 50, clientY: 50, pointerId: 2, pointerType: "mouse" });

    expect(onWorkspaceDrop).not.toHaveBeenCalled();
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(screen.getByTestId("announcement")).toHaveTextContent("Pick cancelled.");
  });

  it("publishes_rows_and_keeps_row_gaps_as_column_only_targets", async () => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(
      <Harness
        interaction={interaction}
        onDrop={onDrop}
        onSettled={vi.fn()}
        expanded
      />,
    );
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    screen.getByTestId("deck-board").getBoundingClientRect = () => rect(0, 0, 200, 200);
    screen.getByTestId("deck-column-0").getBoundingClientRect = () => rect(0, 0, 100, 200);
    screen.getByTestId("deck-column-1").getBoundingClientRect = () => rect(100, 0, 200, 200);
    screen.getByTestId("deck-column-0-row-0").getBoundingClientRect = () => rect(0, 0, 100, 96);
    screen.getByTestId("deck-column-0-row-1").getBoundingClientRect = () => rect(0, 104, 100, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 21, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 50, clientY: 50, pointerId: 21, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("deck-drop-state").textContent!)).toEqual({
      zoneActive: true,
      column: 0,
      row: 0,
    });

    fireEvent.pointerMove(source, { clientX: 50, clientY: 150, pointerId: 21, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("deck-drop-state").textContent!)).toEqual({
      zoneActive: true,
      column: 0,
      row: 1,
    });

    fireEvent.pointerMove(source, { clientX: 50, clientY: 100, pointerId: 21, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("deck-drop-state").textContent!)).toEqual({
      zoneActive: true,
      column: 0,
      row: null,
    });
    fireEvent.pointerUp(source, { clientX: 50, clientY: 100, pointerId: 21, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(onDrop).toHaveBeenCalledWith(expect.objectContaining({
      destination: "deck",
      placementHint: { column: 0 },
    }));
  });

  it("publishes_preview_only_after_threshold_and_keeps_ordered_identity", async () => {
    const interaction = createInteraction();
    let resolveOutcome!: (value: { status: "ignored"; reason: "busy" }) => void;
    const secondCard = { ...card, instance_id: "card-2", name: "Card Two" };
    const sourceOverride: PackDropSource = {
      kind: "draft-effect",
      authorityId: "effect",
      sourceInstanceId: "card-1",
      instanceIds: ["card-1", "card-2"],
      cards: [card, secondCard],
      sourceIndices: [0, 1],
      interactionGeneration: 1,
      previewWidth: 146,
      previewHeight: 204,
      onAdmission: vi.fn(),
      onSettled: vi.fn(),
    };
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => {
      expect(screen.getByTestId("preview")).toHaveTextContent("");
      return {
        requestToken: request.requestToken,
        interactionGeneration: 1,
        outcome: new Promise((resolve) => { resolveOutcome = resolve; }),
      };
    });
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} sourceOverride={sourceOverride} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 2, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 10, pointerId: 2, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 2, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("preview").textContent!)).toEqual({
      ids: ["card-1", "card-2"],
      cards: ["card-1", "card-2"],
      x: 30,
      y: 30,
    });
    fireEvent.pointerMove(source, { clientX: 40, clientY: 45, pointerId: 2, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("preview").textContent!)).toMatchObject({ x: 40, y: 45 });
    fireEvent.pointerUp(source, { clientX: 40, clientY: 45, pointerId: 2, pointerType: "mouse" });
    expect(onDrop).toHaveBeenCalledWith(expect.objectContaining({ source: sourceOverride }));
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    await act(async () => resolveOutcome({ status: "ignored", reason: "busy" }));
    expect(screen.getByTestId("preview")).toHaveTextContent("");
  });

  it("retires_a_session_when_pointer_capture_fails_and_allows_a_fresh_gesture", async () => {
    const interaction = createInteraction();
    const onAdmission = vi.fn();
    const onSettled = vi.fn();
    const onDrop = vi.fn((request) => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored" as const, reason: "busy" as const }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop as never} onAdmission={onAdmission} onSettled={onSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn()
      .mockImplementationOnce(() => { throw new Error("capture denied"); })
      .mockImplementationOnce(() => undefined);
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 3, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 3, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 3, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(onDrop).not.toHaveBeenCalled();
    expect(onAdmission).not.toHaveBeenCalled();

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 4, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 4, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 4, pointerType: "mouse" });
    await act(async () => Promise.resolve());

    expect(source.setPointerCapture).toHaveBeenCalledTimes(2);
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onAdmission).toHaveBeenCalledTimes(1);
    expect(onSettled).toHaveBeenCalledWith({ kind: "outcome", outcome: { status: "ignored", reason: "busy" } });
  });

  it("settles_owned_only_after_the_matching_unlock_and_total_outcome", async () => {
    const interaction = createInteraction();
    let resolveOutcome!: (value: { status: "acknowledged" }) => void;
    const outcome = new Promise<{ status: "acknowledged" }>((resolve) => { resolveOutcome = resolve; });
    const callOrder: string[] = [];
    const onAdmission = vi.fn(() => callOrder.push("admission"));
    const onSettled = vi.fn();
    const onDrop = vi.fn((request): DraftDropDispatch => {
      callOrder.push("drop");
      interaction.publish({ interactionGeneration: 1, pickInteractionLocked: true, pendingPickIntent: { kind: "pick", instanceIds: ["card-1"], destination: "sideboard", placementHint: { column: 2 } } });
      return { requestToken: request.requestToken, interactionGeneration: 1, outcome };
    });
    render(<Harness interaction={interaction} onDrop={onDrop as never} onAdmission={onAdmission} onSettled={onSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn((pointerId: number) => {
      fireEvent.lostPointerCapture(source, { pointerId, pointerType: "mouse" });
    });
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 4, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 4, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 4, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(callOrder).toEqual(["admission", "drop"]);
    expect(onAdmission).toHaveBeenCalledWith(expect.objectContaining({ kind: "dispatch", interactionGeneration: 1 }));
    expect(source.releasePointerCapture).toHaveBeenCalledTimes(1);
    expect(onSettled).not.toHaveBeenCalled();

    await act(async () => resolveOutcome({ status: "acknowledged" }));
    expect(onSettled).not.toHaveBeenCalled();
    act(() => interaction.publish({ interactionGeneration: 1, pickInteractionLocked: false, pendingPickIntent: null }));
    expect(onSettled).toHaveBeenCalledWith({ kind: "outcome", outcome: { status: "acknowledged" } });
  });

  it("settles_clean_unowned_without_waiting_for_a_lock_edge", async () => {
    const interaction = createInteraction();
    const onSettled = vi.fn();
    const onDrop = vi.fn((request) => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "rejected" as const, reason: "invalid-request" as const }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop as never} onSettled={onSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);
    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 5, pointerType: "pen" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 5, pointerType: "pen" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 5, pointerType: "pen" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    await act(async () => Promise.resolve());
    expect(onSettled).toHaveBeenCalledWith({ kind: "outcome", outcome: { status: "rejected", reason: "invalid-request" } });
  });

  it("rejects_a_public_conflict_before_dispatch_without_visual_admission", () => {
    const interaction = createInteraction();
    const onAdmission = vi.fn();
    const onDrop = vi.fn();
    const onSettled = vi.fn();
    render(<Harness interaction={interaction} onDrop={onDrop as never} onAdmission={onAdmission} onSettled={onSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 100, 100);

    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 55, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 55, pointerType: "mouse" });
    act(() => interaction.publish({
      interactionGeneration: 1,
      pickInteractionLocked: true,
      pendingPickIntent: { kind: "pick", instanceIds: ["other"], destination: "deck" },
    }));
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 55, pointerType: "mouse" });

    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(onAdmission).not.toHaveBeenCalled();
    expect(onDrop).not.toHaveBeenCalled();
    expect(onSettled).toHaveBeenCalledWith({ kind: "conflict" });
  });

  it("retires_conflict_and_reports_callback_throw_once", () => {
    const interaction = createInteraction();
    const conflictSettled = vi.fn();
    const conflictDrop = vi.fn((request) => {
      interaction.publish({ interactionGeneration: 1, pickInteractionLocked: true, pendingPickIntent: { kind: "pick", instanceIds: ["other"], destination: "deck" } });
      return { requestToken: request.requestToken, interactionGeneration: 1, outcome: Promise.resolve({ status: "ignored" as const, reason: "busy" as const }) };
    });
    const { unmount } = render(<Harness interaction={interaction} onDrop={conflictDrop as never} onSettled={conflictSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn(); source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 100, 100);
    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 6, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 6, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 6, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(conflictSettled).toHaveBeenCalledWith({ kind: "conflict" });
    unmount();

    const clean = createInteraction();
    const errorSettled = vi.fn();
    render(<Harness interaction={clean} onDrop={(() => { throw new Error("boom"); }) as never} onSettled={errorSettled} />);
    const errorSource = screen.getByTestId("source");
    const errorTarget = screen.getByTestId("target");
    errorSource.setPointerCapture = vi.fn(); errorSource.releasePointerCapture = vi.fn();
    errorTarget.getBoundingClientRect = () => rect(0, 0, 100, 100);
    fireEvent.pointerDown(errorSource, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 7, pointerType: "mouse" });
    fireEvent.pointerMove(errorSource, { clientX: 20, clientY: 20, pointerId: 7, pointerType: "mouse" });
    fireEvent.pointerUp(errorSource, { clientX: 20, clientY: 20, pointerId: 7, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(errorSettled).toHaveBeenCalledWith({ kind: "error" });
    expect(screen.getByTestId("announcement")).toHaveTextContent("Could not submit Card One. Try again.");
  });

  it.each(["mouse", "pen"] as const)("suppresses_the_complete_successful_%s_pack_drag_click_double_click_sequence_until_a_new_pointer_down", async (pointerType) => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 20, pointerType });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 20, pointerType });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 20, pointerType });
    await act(async () => Promise.resolve());
    firePointerActivation(source, "click", { detail: 1, pointerId: 20, pointerType });
    firePointerActivation(source, "click", { detail: 2, pointerId: 20, pointerType });
    fireEvent(source, new MouseEvent("dblclick", { bubbles: true, detail: 2 }));
    expect(screen.getByTestId("clicks")).toHaveTextContent("0:0");

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 21, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 10, clientY: 10, pointerId: 21, pointerType: "mouse" });
    fireEvent.click(source);
    fireEvent.doubleClick(source);
    expect(screen.getByTestId("clicks")).toHaveTextContent("1:1");
  });

  it("captures_and_releases_a_pointer_for_a_stationary_mouse_click", () => {
    const interaction = createInteraction();
    render(<Harness interaction={interaction} onDrop={vi.fn() as never} onSettled={vi.fn()} />);
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 24, pointerType: "mouse" });
    expect(source.setPointerCapture).toHaveBeenCalledWith(24);
    fireEvent.pointerUp(source, { clientX: 10, clientY: 10, pointerId: 24, pointerType: "mouse" });
    fireEvent.click(source, { detail: 1, pointerType: "mouse" });

    expect(source.releasePointerCapture).toHaveBeenCalledWith(24);
    expect(screen.getByTestId("clicks")).toHaveTextContent("1:0");
  });

  it.each(["mouse", "pen"] as const)("does_not_swallow_keyboard_activation_after_one_%s_compatibility_click", async (pointerType) => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 22, pointerType });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 22, pointerType });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 22, pointerType });
    await act(async () => Promise.resolve());
    firePointerActivation(source, "click", { detail: 1, pointerId: 22, pointerType });
    expect(screen.getByTestId("clicks")).toHaveTextContent("0:0");

    fireEvent.click(source, { detail: 0 });
    expect(screen.getByTestId("clicks")).toHaveTextContent("1:0");
    fireEvent.click(source, { detail: 1, pointerType });
    expect(screen.getByTestId("clicks")).toHaveTextContent("2:0");
  });

  it("retires_compatibility_tokens_for_keyboard_missing_and_mismatched_pointer_identity", async () => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    const drag = async (pointerId: number) => {
      fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId, pointerType: "mouse" });
      fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId, pointerType: "mouse" });
      fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId, pointerType: "mouse" });
      await act(async () => Promise.resolve());
    };

    await drag(40);
    firePointerActivation(source, "click", { detail: 0, pointerId: 40, pointerType: "mouse" });
    await drag(41);
    fireEvent.click(source, { detail: 1, pointerType: "mouse" });
    await drag(42);
    firePointerActivation(source, "click", { detail: 1, pointerId: 43, pointerType: "mouse" });
    await drag(44);
    firePointerActivation(source, "click", { detail: 1, pointerId: 44, pointerType: "pen" });

    expect(screen.getByTestId("clicks")).toHaveTextContent("4:0");
  });

  it.each([
    { surface: "workspace" as const, sourceInstanceId: "card-2", name: "workspace B" },
    { surface: "pack" as const, sourceInstanceId: "card-1", name: "pack" },
  ])("allows_a_stale_workspace_A_token_on_$name_and_retires_it", async (secondaryActivation) => {
    const interaction = createInteraction();
    render(
      <Harness
        interaction={interaction}
        onDrop={vi.fn() as never}
        onSettled={vi.fn()}
        workspaceSourceOverride={{
          kind: "workspace", instanceIds: ["card-1"], cards: [card], previewWidth: 146, previewHeight: 204, onDrop: () => true,
        }}
        secondaryActivation={secondaryActivation}
      />,
    );
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 200, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 50, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 50, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 50, pointerType: "mouse" });
    firePointerActivation(screen.getByTestId("secondary-activation"), "click", { detail: 1, pointerId: 50, pointerType: "mouse" });
    firePointerActivation(source, "click", { detail: 1, pointerId: 50, pointerType: "mouse" });

    expect(screen.getByTestId("clicks")).toHaveTextContent("2:0");
  });

  it("leaves_touch_activation_outside_drag_compatibility_suppression", () => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} />);
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 23, pointerType: "touch" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 23, pointerType: "touch" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 23, pointerType: "touch" });
    fireEvent.click(source, { detail: 1, pointerType: "touch" });

    expect(source.setPointerCapture).not.toHaveBeenCalled();
    expect(onDrop).not.toHaveBeenCalled();
    expect(screen.getByTestId("clicks")).toHaveTextContent("1:0");
  });

  it("keeps_touch_workspace_compatibility_activation_suppressed_after_a_no_target_drag_release", () => {
    const interaction = createInteraction();
    const onWorkspaceDrop = vi.fn(() => true);
    render(
      <Harness
        interaction={interaction}
        onDrop={vi.fn() as never}
        onSettled={vi.fn()}
        workspaceTouchEnabled
        workspaceSourceOverride={{
          kind: "workspace", instanceIds: ["card-1"], cards: [card], previewWidth: 146, previewHeight: 204, onDrop: onWorkspaceDrop,
        }}
      />,
    );
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(100, 0, 300, 200);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 25, pointerType: "touch" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 25, pointerType: "touch" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 25, pointerType: "touch" });
    firePointerActivation(source, "click", { detail: 1, pointerId: 25, pointerType: "touch" });

    expect(onWorkspaceDrop).not.toHaveBeenCalled();
    expect(screen.getByTestId("clicks")).toHaveTextContent("0:0");
  });

  it("clips_expanded_columns_to_their_board_and_rejects_release_outside_the_board", async () => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} expanded />);
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    screen.getByTestId("deck-board").getBoundingClientRect = () => rect(0, 0, 100, 100);
    screen.getByTestId("deck-column-0").getBoundingClientRect = () => rect(-100, 0, 20, 100);
    screen.getByTestId("deck-column-1").getBoundingClientRect = () => rect(80, 0, 200, 100);
    screen.getByTestId("sideboard-board").getBoundingClientRect = () => rect(300, 0, 400, 100);
    screen.getByTestId("sideboard-column-0").getBoundingClientRect = () => rect(300, 0, 400, 100);

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 30, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 90, clientY: 50, pointerId: 30, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 90, clientY: 50, pointerId: 30, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(onDrop).toHaveBeenLastCalledWith(expect.objectContaining({ destination: "deck", placementHint: { column: 1 } }));

    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 31, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 110, clientY: 50, pointerId: 31, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 110, clientY: 50, pointerId: 31, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(onDrop).toHaveBeenCalledTimes(1);
  });

  it("rejects_inter_column_gaps_and_orders_only_overlapping_containing_columns", async () => {
    const interaction = createInteraction();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken,
      interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} expanded />);
    const source = screen.getByTestId("source");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    screen.getByTestId("deck-board").getBoundingClientRect = () => rect(0, 0, 200, 100);
    screen.getByTestId("deck-column-0").getBoundingClientRect = () => rect(0, 0, 40, 100);
    screen.getByTestId("deck-column-1").getBoundingClientRect = () => rect(160, 0, 200, 100);
    screen.getByTestId("sideboard-board").getBoundingClientRect = () => rect(300, 0, 400, 100);
    screen.getByTestId("sideboard-column-0").getBoundingClientRect = () => rect(300, 0, 400, 100);

    fireEvent.pointerDown(source, { button: 0, clientX: 0, clientY: 0, isPrimary: true, pointerId: 40, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 100, clientY: 50, pointerId: 40, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("deck-drop-state").textContent!)).toEqual({
      zoneActive: false,
      column: null,
      row: null,
    });
    fireEvent.pointerUp(source, { clientX: 100, clientY: 50, pointerId: 40, pointerType: "mouse" });
    expect(onDrop).not.toHaveBeenCalled();

    fireEvent.pointerDown(source, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 41, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 39, clientY: 50, pointerId: 41, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("deck-drop-state").textContent!)).toEqual({ zoneActive: true, column: 0, row: null });
    fireEvent.pointerUp(source, { clientX: 39, clientY: 50, pointerId: 41, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(onDrop).toHaveBeenLastCalledWith(expect.objectContaining({ destination: "deck", placementHint: { column: 0 } }));

    fireEvent.pointerDown(source, { button: 0, clientX: 180, clientY: 20, isPrimary: true, pointerId: 42, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 161, clientY: 50, pointerId: 42, pointerType: "mouse" });
    expect(JSON.parse(screen.getByTestId("deck-drop-state").textContent!)).toEqual({ zoneActive: true, column: 1, row: null });
    fireEvent.pointerUp(source, { clientX: 161, clientY: 50, pointerId: 42, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(onDrop).toHaveBeenLastCalledWith(expect.objectContaining({ destination: "deck", placementHint: { column: 1 } }));

    screen.getByTestId("deck-column-1").getBoundingClientRect = () => rect(0, 0, 40, 100);
    screen.getByTestId("sideboard-board").getBoundingClientRect = () => rect(0, 0, 200, 100);
    screen.getByTestId("sideboard-column-0").getBoundingClientRect = () => rect(0, 0, 40, 100);
    fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 43, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 43, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 43, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(onDrop).toHaveBeenLastCalledWith(expect.objectContaining({ destination: "deck", placementHint: { column: 0 } }));
  });

  it("handles_pending_up_and_invalid_release_without_submission_then_reaches_a_valid_drop", async () => {
    const interaction = createInteraction();
    const onAdmission = vi.fn();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken, interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    }));
    render(<Harness interaction={interaction} onDrop={onDrop} onAdmission={onAdmission} onSettled={vi.fn()} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 100, 100);

    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 50, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 5, clientY: 5, pointerId: 50, pointerType: "mouse" });
    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 51, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 51, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 200, clientY: 200, pointerId: 51, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(onDrop).not.toHaveBeenCalled();
    expect(onAdmission).not.toHaveBeenCalled();

    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 52, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 52, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 52, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onAdmission).toHaveBeenCalledTimes(1);
    expect(source.releasePointerCapture).toHaveBeenCalledTimes(3);
  });

  it("distinguishes_pending_dragging_and_settling_lost_capture", async () => {
    const interaction = createInteraction();
    let resolveOutcome!: (value: { status: "ignored"; reason: "busy" }) => void;
    const onSettled = vi.fn();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => {
      fireEvent.lostPointerCapture(screen.getByTestId("source"), { pointerId: 62, pointerType: "mouse" });
      return {
        requestToken: request.requestToken, interactionGeneration: 1,
        outcome: new Promise((resolve) => { resolveOutcome = resolve; }),
      };
    });
    render(<Harness interaction={interaction} onDrop={onDrop} onSettled={onSettled} />);
    const sourceElement = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    sourceElement.setPointerCapture = vi.fn();
    sourceElement.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 100, 100);

    fireEvent.pointerDown(sourceElement, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 60, pointerType: "mouse" });
    fireEvent.lostPointerCapture(sourceElement, { pointerId: 60, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(screen.getByTestId("announcement")).toHaveTextContent("");
    fireEvent.pointerDown(sourceElement, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 61, pointerType: "mouse" });
    fireEvent.pointerMove(sourceElement, { clientX: 20, clientY: 20, pointerId: 61, pointerType: "mouse" });
    fireEvent.lostPointerCapture(sourceElement, { pointerId: 61, pointerType: "mouse" });
    fireEvent.lostPointerCapture(sourceElement, { pointerId: 61, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(screen.getByTestId("announcement")).toHaveTextContent("Pick cancelled.");

    fireEvent.pointerDown(sourceElement, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 62, pointerType: "mouse" });
    fireEvent.pointerMove(sourceElement, { clientX: 20, clientY: 20, pointerId: 62, pointerType: "mouse" });
    fireEvent.pointerUp(sourceElement, { clientX: 20, clientY: 20, pointerId: 62, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    await act(async () => resolveOutcome({ status: "ignored", reason: "busy" }));
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onSettled).toHaveBeenCalledWith({ kind: "outcome", outcome: { status: "ignored", reason: "busy" } });
  });

  it("retires_on_cancel_generation_replacement_and_rejected_dispatch_with_positive_reach", async () => {
    const interaction = createInteraction();
    const onSettled = vi.fn();
    const rejectedDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken, interactionGeneration: 1,
      outcome: Promise.reject(new Error("offline")),
    }));
    render(<Harness interaction={interaction} onDrop={rejectedDrop} onSettled={onSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 100, 100);

    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 70, pointerType: "mouse" });
    fireEvent.pointerCancel(source, { pointerId: 70, pointerType: "mouse" });
    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 71, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 71, pointerType: "mouse" });
    fireEvent.pointerCancel(source, { pointerId: 71, pointerType: "mouse" });
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(screen.getByTestId("announcement")).toHaveTextContent("Pick cancelled.");
    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 72, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 72, pointerType: "mouse" });
    act(() => interaction.publish({ interactionGeneration: 2, pickInteractionLocked: false, pendingPickIntent: null }));
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 72, pointerType: "mouse" });
    expect(rejectedDrop).not.toHaveBeenCalled();

    act(() => interaction.publish({ interactionGeneration: 1, pickInteractionLocked: false, pendingPickIntent: null }));
    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 73, pointerType: "mouse" });
    fireEvent.pointerMove(source, { clientX: 20, clientY: 20, pointerId: 73, pointerType: "mouse" });
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 73, pointerType: "mouse" });
    await act(async () => Promise.resolve());
    expect(rejectedDrop).toHaveBeenCalledTimes(1);
    expect(onSettled).toHaveBeenCalledWith({ kind: "error" });
    expect(screen.getByTestId("announcement")).toHaveTextContent("Could not submit Card One. Try again.");
  });

  it("retires_pending_and_settling_work_on_disable_dispose_and_unmount", async () => {
    const interaction = createInteraction();
    let resolveOutcome!: (value: { status: "ignored"; reason: "busy" }) => void;
    const onSettled = vi.fn();
    const onDrop = vi.fn((request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken, interactionGeneration: 1,
      outcome: new Promise((resolve) => { resolveOutcome = resolve; }),
    }));
    const rendered = render(<Harness interaction={interaction} onDrop={onDrop} onSettled={onSettled} />);
    const source = screen.getByTestId("source");
    const target = screen.getByTestId("target");
    source.setPointerCapture = vi.fn();
    source.releasePointerCapture = vi.fn();
    target.getBoundingClientRect = () => rect(0, 0, 100, 100);
    fireEvent.pointerDown(source, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 80, pointerType: "mouse" });
    rendered.rerender(<Harness interaction={interaction} onDrop={onDrop} onSettled={onSettled} enabled={false} />);
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    fireEvent.pointerUp(source, { clientX: 20, clientY: 20, pointerId: 80, pointerType: "mouse" });
    expect(onDrop).not.toHaveBeenCalled();

    rendered.rerender(<Harness interaction={interaction} onDrop={onDrop} onSettled={onSettled} />);
    const freshSource = screen.getByTestId("source");
    const freshTarget = screen.getByTestId("target");
    freshSource.setPointerCapture = vi.fn(); freshSource.releasePointerCapture = vi.fn();
    freshTarget.getBoundingClientRect = () => rect(0, 0, 100, 100);
    fireEvent.pointerDown(freshSource, { button: 0, clientX: 5, clientY: 5, isPrimary: true, pointerId: 81, pointerType: "mouse" });
    fireEvent.pointerMove(freshSource, { clientX: 20, clientY: 20, pointerId: 81, pointerType: "mouse" });
    fireEvent.pointerUp(freshSource, { clientX: 20, clientY: 20, pointerId: 81, pointerType: "mouse" });
    fireEvent.click(screen.getByRole("button", { name: "dispose" }));
    expect(screen.getByTestId("preview")).toHaveTextContent("");
    expect(onSettled).toHaveBeenCalledWith({ kind: "conflict" });
    await act(async () => resolveOutcome({ status: "ignored", reason: "busy" }));
    expect(onSettled).toHaveBeenCalledTimes(1);
    rendered.unmount();
    expect(interaction.listenerCount()).toBe(0);
  });

  it("balances_listeners_observers_subscriptions_and_ref_replacement_in_strict_mode", () => {
    const interaction = createInteraction();
    const add = vi.spyOn(window, "addEventListener");
    const remove = vi.spyOn(window, "removeEventListener");
    const observe = vi.fn();
    const unobserve = vi.fn();
    const disconnect = vi.fn();
    const OriginalResizeObserver = globalThis.ResizeObserver;
    globalThis.ResizeObserver = class {
      observe = observe;
      unobserve = unobserve;
      disconnect = disconnect;
    } as unknown as typeof ResizeObserver;
    const onDrop = (request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken, interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    });
    const rendered = render(<StrictMode><Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} targetVersion={0} /></StrictMode>);
    expect(interaction.listenerCount()).toBe(1);
    rendered.rerender(<StrictMode><Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} targetVersion={1} /></StrictMode>);
    expect(unobserve).toHaveBeenCalled();
    rendered.unmount();
    expect(interaction.listenerCount()).toBe(0);
    expect(disconnect).toHaveBeenCalled();
    expect(add.mock.calls.filter(([type]) => type === "scroll")).toHaveLength(remove.mock.calls.filter(([type]) => type === "scroll").length);
    expect(add.mock.calls.filter(([type]) => type === "resize")).toHaveLength(remove.mock.calls.filter(([type]) => type === "resize").length);
    globalThis.ResizeObserver = OriginalResizeObserver;
    add.mockRestore();
    remove.mockRestore();
  });

  it("observes_pre_effect_targets_then_balances_replacement_and_cleanup", () => {
    const interaction = createInteraction();
    const observe = vi.fn();
    const unobserve = vi.fn();
    const disconnect = vi.fn();
    const OriginalResizeObserver = globalThis.ResizeObserver;
    globalThis.ResizeObserver = class {
      observe = observe;
      unobserve = unobserve;
      disconnect = disconnect;
    } as unknown as typeof ResizeObserver;
    const onDrop = (request: DraftDropRequest): DraftDropDispatch => ({
      requestToken: request.requestToken, interactionGeneration: 1,
      outcome: Promise.resolve({ status: "ignored", reason: "busy" }),
    });

    const rendered = render(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} targetVersion={0} />);
    const initialTarget = screen.getByTestId("target");
    expect(observe).toHaveBeenCalledTimes(1);
    expect(observe).toHaveBeenCalledWith(initialTarget);

    rendered.rerender(<Harness interaction={interaction} onDrop={onDrop} onSettled={vi.fn()} targetVersion={1} />);
    const replacementTarget = screen.getByTestId("target");
    expect(unobserve).toHaveBeenCalledWith(initialTarget);
    expect(observe).toHaveBeenCalledWith(replacementTarget);
    expect(observe).toHaveBeenCalledTimes(2);

    rendered.unmount();
    expect(unobserve).toHaveBeenCalledWith(replacementTarget);
    expect(disconnect).toHaveBeenCalledOnce();
    globalThis.ResizeObserver = OriginalResizeObserver;
  });
});
