import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createRef, useState } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { GameMenu } from "../GameMenu";
import { ConcedeDialog } from "../../multiplayer/ConcedeDialog";
import { ModalPanelShell } from "../../ui/ModalPanelShell";

vi.mock("../../../hooks/useCardDataMeta.ts", () => ({
  useCardDataMeta: () => null,
}));

function renderGameMenu(
  props: Partial<React.ComponentProps<typeof GameMenu>> = {},
) {
  const onToggleMultiplayerBoardLayout = vi.fn();

  render(
    <MemoryRouter initialEntries={["/game/test-game"]}>
      <GameMenu
        gameId="test-game"
        isAiMode={false}
        isOnlineMode={false}
        showAiHand={false}
        onToggleAiHand={vi.fn()}
        logPanelOpen={false}
        onToggleGameLog={vi.fn()}
        onSettingsClick={vi.fn()}
        onHelpClick={vi.fn()}
        multiplayerBoardLayout="split"
        onToggleMultiplayerBoardLayout={onToggleMultiplayerBoardLayout}
        {...props}
      />
    </MemoryRouter>,
  );

  return { onToggleMultiplayerBoardLayout };
}

describe("GameMenu", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the split/legacy board layout toggle inside the menu", () => {
    const { onToggleMultiplayerBoardLayout } = renderGameMenu();

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Switch to legacy focused view Split" }));

    expect(onToggleMultiplayerBoardLayout).toHaveBeenCalledTimes(1);
  });

  it("provides its own touch scroll region on short game viewports", () => {
    renderGameMenu();

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    const menuPanel = screen.getByRole("button", { name: "Concede" }).closest(
      '[aria-label="Game menu"]',
    );

    expect(menuPanel).toHaveClass(
      "game-menu-scroll",
      "overflow-y-auto",
      "overscroll-contain",
      "touch-pan-y",
    );
  });

  it("exposes the production hamburger through a shared trigger ref", () => {
    const menuTriggerRef = createRef<HTMLButtonElement>();
    renderGameMenu({ menuTriggerRef });

    expect(menuTriggerRef.current).toBe(
      screen.getByRole("button", { name: "Game menu" }),
    );
  });

  it("labels the legacy state with the split destination", () => {
    renderGameMenu({ multiplayerBoardLayout: "focused" });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Switch to split table view Legacy" })).toBeInTheDocument();
  });

  it("routes the split-layout nudge through its supplied callbacks", () => {
    const onTryMultiplayerSplitLayout = vi.fn();
    const onDismissMultiplayerSplitLayoutNudge = vi.fn();
    renderGameMenu({
      showMultiplayerSplitLayoutNudge: true,
      onTryMultiplayerSplitLayout,
      onDismissMultiplayerSplitLayoutNudge,
    });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Try split view" }));

    expect(onTryMultiplayerSplitLayout).toHaveBeenCalledOnce();
    expect(onDismissMultiplayerSplitLayoutNudge).not.toHaveBeenCalled();
  });

  it("hides the split-layout nudge without its complete callback contract", () => {
    renderGameMenu({ showMultiplayerSplitLayoutNudge: true });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.queryByRole("button", { name: "Try split view" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
  });

  it("opens sandbox tools from the collapsed menu", () => {
    const onSandboxToolsClick = vi.fn();
    renderGameMenu({ showSandboxTools: true, onSandboxToolsClick });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Open Sandbox Tools" }));

    expect(onSandboxToolsClick).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Open Sandbox Tools" })).not.toBeInTheDocument();
  });

  it("restores the hamburger after Settings replaces its transient menu item", async () => {
    function SettingsHarness() {
      const [settingsOpen, setSettingsOpen] = useState(false);
      return (
        <MemoryRouter initialEntries={["/game/test-game"]}>
          <GameMenu
            gameId="test-game"
            isAiMode={false}
            isOnlineMode={false}
            showAiHand={false}
            onToggleAiHand={vi.fn()}
            logPanelOpen={false}
            onToggleGameLog={vi.fn()}
            onSettingsClick={() => setSettingsOpen(true)}
            onHelpClick={vi.fn()}
          />
          <ModalPanelShell
            open={settingsOpen}
            title="Settings"
            onClose={() => setSettingsOpen(false)}
          >
            <button type="button">Save settings</button>
          </ModalPanelShell>
        </MemoryRouter>
      );
    }

    render(<SettingsHarness />);
    const trigger = screen.getByRole("button", { name: "Game menu" });
    fireEvent.click(trigger);
    const settingsItem = screen.getByRole("button", { name: "Settings" });
    settingsItem.focus();
    fireEvent.click(settingsItem);

    const dialog = screen.getByRole("dialog", { name: "Settings" });
    await waitFor(() => expect(dialog).toHaveFocus());
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Settings" })).not.toBeInTheDocument(),
    );
    expect(trigger).toHaveFocus();
  });

  it("moves focus to the hamburger before opening menu-owned surfaces", () => {
    const onReportCardClick = vi.fn();
    const onHelpClick = vi.fn();
    renderGameMenu({ showReportCard: true, onReportCardClick, onHelpClick });

    const trigger = screen.getByRole("button", { name: "Game menu" });
    fireEvent.click(trigger);
    const reportItem = screen.getByRole("button", { name: "Report a card" });
    reportItem.focus();
    fireEvent.click(reportItem);

    expect(onReportCardClick).toHaveBeenCalledOnce();
    expect(trigger).toHaveFocus();

    fireEvent.click(trigger);
    const helpItem = screen.getByRole("button", { name: "Help & Shortcuts ?" });
    helpItem.focus();
    fireEvent.click(helpItem);

    expect(onHelpClick).toHaveBeenCalledOnce();
    expect(trigger).toHaveFocus();
  });

  it("restores the hamburger after the online concede confirmation closes", async () => {
    function OnlineConcedeHarness() {
      const [open, setOpen] = useState(false);
      return (
        <MemoryRouter initialEntries={["/game/test-game"]}>
          <GameMenu
            gameId="test-game"
            isAiMode={false}
            isOnlineMode
            showAiHand={false}
            onToggleAiHand={vi.fn()}
            logPanelOpen={false}
            onToggleGameLog={vi.fn()}
            onSettingsClick={vi.fn()}
            onHelpClick={vi.fn()}
            onConcede={() => setOpen(true)}
          />
          <ConcedeDialog
            isOpen={open}
            gameAction={{
              kind: "game",
              consequence: "ordinary-game",
              onConfirm: vi.fn(),
            }}
            onCancel={() => setOpen(false)}
          />
        </MemoryRouter>
      );
    }
    render(<OnlineConcedeHarness />);

    const trigger = screen.getByRole("button", { name: "Game menu" });
    fireEvent.click(trigger);
    const concedeItem = screen.getByRole("button", { name: "Concede" });
    concedeItem.focus();
    fireEvent.click(concedeItem);

    const dialog = await screen.findByRole("alertdialog", { name: "Concede Game?" });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog", { name: "Concede Game?" })).not.toBeInTheDocument(),
    );
    expect(trigger).toHaveFocus();
  });

  it("toggles the floating click mode button from the collapsed menu", () => {
    const onToggleDebugClickModeButtonVisible = vi.fn();
    renderGameMenu({
      showSandboxTools: true,
      onSandboxToolsClick: vi.fn(),
      onToggleDebugClickModeButtonVisible,
    });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Click Mode Button Hidden" }));

    expect(onToggleDebugClickModeButtonVisible).toHaveBeenCalledTimes(1);
  });

  it("shows the floating click mode button as pinned in the collapsed menu", () => {
    renderGameMenu({
      showSandboxTools: true,
      onSandboxToolsClick: vi.fn(),
      debugClickModeButtonVisible: true,
      onToggleDebugClickModeButtonVisible: vi.fn(),
    });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Click Mode Button Shown" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  // The takeback item's gate used to be `isOnlineMode && onRequestTakeback`,
  // which hid it for desktop solo-vs-AI (`native-ai` reaches GamePage as
  // `mode=ai`, so `isOnlineMode` is false) even though that game has a real
  // server-authoritative takeback and no client-side undo. GamePage is now the
  // single authority and gates on the transport; the menu only asks whether it
  // was given a handler.
  it("offers takeback whenever a handler is supplied, even outside online mode", () => {
    const onRequestTakeback = vi.fn();
    // `isOnlineMode: false` is the whole point — this is the desktop-solo
    // shape, and it is what fails if the `isOnlineMode &&` gate comes back.
    renderGameMenu({ isOnlineMode: false, onRequestTakeback });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Request Takeback" }));

    expect(onRequestTakeback).toHaveBeenCalledTimes(1);
  });

  it("hides takeback when no handler is supplied", () => {
    // Paired negative: without it, "render always" would pass the test above.
    renderGameMenu({ isOnlineMode: true, onRequestTakeback: undefined });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    // Reach guard: the menu really did open, so the absence below is the gate
    // rather than an unrendered menu.
    expect(screen.getByRole("button", { name: "Concede" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Request Takeback" })).toBeNull();
  });

  // F4. "Request Takeback" is a proposal addressed to other humans. Solo vs.
  // AI there is nobody to ask and the server auto-approves, so the same entry
  // must read as a plain undo. Only the label changes — the handler and the
  // wire frame are identical.
  it("labels the rollback entry as an undo when the audience is solo", () => {
    const onRequestTakeback = vi.fn();
    renderGameMenu({ onRequestTakeback, takebackAudience: "solo" });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Undo Last Action" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Request Takeback" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Undo Last Action" }));
    expect(onRequestTakeback).toHaveBeenCalledTimes(1);
  });

  it("keeps the table wording when other humans are present", () => {
    // Paired positive. Also covers the default: `takebackAudience` is omitted
    // here, and the default must be the pre-existing wording.
    renderGameMenu({ onRequestTakeback: vi.fn() });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Request Takeback" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Undo Last Action" })).toBeNull();
  });

  it("renders no rollback entry at all without a handler, whatever the audience", () => {
    // Hostile: the audience must not become a second render gate.
    renderGameMenu({ onRequestTakeback: undefined, takebackAudience: "solo" });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Concede" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Undo Last Action" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Request Takeback" })).toBeNull();
  });

  it("replaces the no-op Resume entry with a game log toggle", () => {
    const onToggleGameLog = vi.fn();
    renderGameMenu({ onToggleGameLog });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open Game Log" }));

    expect(onToggleGameLog).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Open Game Log" })).not.toBeInTheDocument();
  });

  it("offers to close the game log while it is open", () => {
    renderGameMenu({ logPanelOpen: true });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Close Game Log" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open Game Log" })).toBeNull();
  });
});
