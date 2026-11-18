import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import type { ParsedItem } from "../../../hooks/useEngineCardData.ts";
import { trackEvent } from "../../../services/telemetry.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { CardReportDialog } from "../CardReportDialog.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({ useCardImage: vi.fn() }));
vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: vi.fn(() => ({ handlers: {}, firedRef: { current: false } })),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  // One supported item of two, so an idle row shows the fraction "1/2".
  useCardParseDetails: vi.fn((): ParsedItem[] => [
    { category: "trigger", label: "Landfall", supported: true },
    { category: "keyword", label: "Flying", supported: false },
  ]),
}));

// Telemetry is the boundary: each case decides whether the event was queued.
vi.mock("../../../services/telemetry.ts", () => ({
  trackEvent: vi.fn(),
}));

/**
 * Open the picker on one visible battlefield card. Every case uses its own
 * oracle id: the reported-cards store lives for the session and is never
 * reset, so a shared card would carry one case's "Reported" into the next.
 */
function openPickerOn(oracleId: string): void {
  const card = gameObjectFactory
    .onBattlefield()
    .named("Starship Enterprise")
    .ownedBy(0)
    .build({
      display_visible_to_viewer: true,
      printed_ref: { oracle_id: oracleId, face_name: "Starship Enterprise" },
    });
  useGameStore.setState({
    gameMode: "local",
    gameState: gameStateFactory.withPlayers(0, 1).withObjects(card).build(),
  });
  useUiStore.setState({ cardReportDialogOpen: true });
  render(<CardReportDialog />);
}

/** Tap the card's row, then confirm the report. */
function reportTheCard(): void {
  fireEvent.click(screen.getByRole("button", { name: /Starship Enterprise/ }));
  fireEvent.click(screen.getByRole("button", { name: "Report a problem" }));
}

beforeEach(() => {
  vi.mocked(useCardImage).mockReturnValue({
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useUiStore.setState({ cardReportDialogOpen: false });
  useGameStore.setState({ gameState: null, gameMode: null });
});

// Regression tests for #11001: the picker locked a row as "Reported" before
// telemetry had queued anything, so a dropped event still read as sent.
describe("CardReportDialog report status", () => {
  it("locks the row as Reported once telemetry queued the event", () => {
    vi.mocked(trackEvent).mockReturnValue(true);
    openPickerOn("oracle-picker-queued");

    reportTheCard();

    expect(trackEvent).toHaveBeenCalledTimes(1);
    expect(screen.getByText("Reported ✓")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Starship Enterprise/ })).toBeNull();
  });

  it("says Not sent for a report telemetry dropped, and keeps the row tappable", () => {
    vi.mocked(trackEvent).mockReturnValue(false);
    openPickerOn("oracle-picker-dropped");
    // Before the report, the idle row shows the parse fraction.
    expect(screen.getByRole("button", { name: /Starship Enterprise/ })).toHaveTextContent("1/2");

    reportTheCard();

    expect(trackEvent).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(/Reported/)).toBeNull();
    const row = screen.getByRole("button", { name: /Starship Enterprise/ });
    expect(row).toHaveTextContent("Not sent");
    // The label takes the parse fraction's place.
    expect(row).not.toHaveTextContent("1/2");
    expect(row).toBeEnabled();
  });
});
