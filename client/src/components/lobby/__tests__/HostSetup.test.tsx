import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import i18n from "i18next";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// A real `localStorage` for the store's `persist` middleware and for the
// saved-custom-format service, installed before the modules under test are
// imported. Mirrors the stub `multiplayerStore.test.ts` already uses: on some
// Node versions a built-in WebStorage global shadows the DOM environment's
// `localStorage` with a method-less object, and zustand's persist then throws
// "storage.setItem is not a function" on the first `setState`.
const localStorageItems = vi.hoisted(() => {
  const items = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => {
        items.set(key, value);
      },
      removeItem: (key: string) => {
        items.delete(key);
      },
      clear: () => {
        items.clear();
      },
      key: (index: number) => [...items.keys()][index] ?? null,
      get length() {
        return items.size;
      },
    },
  });
  return items;
});

// The saved-format select flow resolves through the WASM adapter. No real
// engine runs in this environment; stub the one method HostSetup calls.
vi.mock("../../../adapter/wasm-adapter", () => ({
  getHostAdapter: () => ({
    formatConfigForCustomRules: vi.fn().mockResolvedValue({
      format: "Custom:0",
      starting_life: 20,
      min_players: 2,
      max_players: 4,
      deck_size: { type: "Minimum", data: 60 },
      singleton: false,
      command_zone: false,
      commander_damage_threshold: null,
      range_of_influence: null,
      team_based: false,
      uses_commander: false,
      supplies_fixed_deck: false,
      sideboard_policy: { type: "Limited", data: 15 },
      default_deck_copy_limit: { type: "UpTo", data: 4 },
      allow_debug_actions: false,
      custom_rules: {
        id: 0,
        structural: {
          starting_life: 20,
          min_players: 2,
          max_players: 4,
          deck_size: { type: "Minimum", data: 60 },
          singleton: false,
          command_zone_mode: "Disabled",
          range_of_influence: null,
          team_based: false,
          sideboard_policy: { type: "Limited", data: 15 },
          default_deck_copy_limit: { type: "UpTo", data: 4 },
        },
        legality: {
          legal_sets: null,
          banned: [],
          restricted: [],
          legacy: {
            mana_burn: "Modern",
            damage_timing: "Modern",
            wish_scope: "PostM10SideboardOnly",
            legend_rule_scope: "Modern",
          },
        },
      },
    }),
  }),
}));

import { HostSetup } from "../HostSetup";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import { saveCustomFormat } from "../../../services/customFormats";
import enMultiplayer from "../../../i18n/locales/en/multiplayer.json";
import deMultiplayer from "../../../i18n/locales/de/multiplayer.json";

describe("HostSetup", () => {
  beforeEach(() => {
    localStorageItems.clear();
    useMultiplayerStore.setState({
      displayName: "",
      formatConfig: null,
      lastHostConfig: null,
    });
  });

  afterEach(async () => {
    cleanup();
    await i18n.changeLanguage("en");
  });

  it("uses P2P labeling/theme and hides server-only lobby listing in p2p mode", () => {
    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="p2p"
      />,
    );

    // The screen heading now lives on the page shell (MultiplayerPage); the
    // form itself is distinguished by its P2P submit-button labeling.
    expect(screen.getByRole("button", { name: "Host P2P Game" })).toBeInTheDocument();
    expect(screen.queryByText("List in lobby")).not.toBeInTheDocument();
    expect(screen.queryByText("P2P currently supports 2-player Standard.")).not.toBeInTheDocument();
  });

  it("keeps server labeling and lobby listing in server mode", () => {
    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    // Heading now lives on the page shell; the form is distinguished by its
    // server-mode submit button + the server-only "List in lobby" toggle.
    expect(screen.getByRole("button", { name: "Host Game" })).toBeInTheDocument();
    expect(screen.getByText("List in lobby")).toBeInTheDocument();
  });

  describe.each(["server", "p2p"] as const)("accessible hosting options (%s mode)", (connectionMode) => {
    it("names every visible switch and associates only the sandbox help", () => {
      render(<HostSetup onHost={vi.fn()} onBack={vi.fn()} connectionMode={connectionMode} />);

      const names = ["Start when full", "Sandbox Mode — allow debug actions", "Set password"];
      if (connectionMode === "server") names.unshift("List in lobby");

      expect(screen.getAllByRole("switch")).toHaveLength(names.length);
      for (const name of names) {
        const control = screen.getByRole("switch", { name });
        if (name === "Sandbox Mode — allow debug actions") {
          expect(control).toHaveAccessibleDescription(enMultiplayer.hostSetup.sandboxModeHelp);
        } else {
          expect(control).not.toHaveAttribute("aria-describedby");
          expect(control).not.toHaveAccessibleDescription();
        }
      }
      if (connectionMode === "p2p") {
        expect(screen.queryByRole("switch", { name: "List in lobby" })).not.toBeInTheDocument();
      }
    });

    it("keeps the compact track inside a non-shrinking touch target", async () => {
      const user = userEvent.setup();
      const onHost = vi.fn();
      render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode={connectionMode} />);

      for (const control of screen.getAllByRole("switch")) {
        // Happy DOM does not lay out CSS. Pin the sizing contract here; check
        // rendered dimensions and clicks outside the track in a real browser.
        expect(control).toHaveClass("min-h-11", "min-w-11", "shrink-0");
        const track = control.firstElementChild;
        expect(track).toHaveAttribute("aria-hidden", "true");
        expect(track).toHaveClass("h-6", "w-[42px]");
        if (!track) throw new Error("Switch track is missing");

        const wasChecked = control.getAttribute("aria-checked");
        await user.click(control);
        expect(control).toHaveAttribute("aria-checked", wasChecked === "true" ? "false" : "true");
        await user.click(track);
        expect(control).toHaveAttribute("aria-checked", wasChecked);
      }
      expect(onHost).not.toHaveBeenCalled();
    });

    it("supports native Space and Enter without submitting until Host is activated", async () => {
      const user = userEvent.setup();
      const onHost = vi.fn();
      render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode={connectionMode} />);

      if (connectionMode === "server") {
        const publicSwitch = screen.getByRole("switch", { name: "List in lobby" });
        expect(publicSwitch).toBeChecked();
        await user.click(publicSwitch);
        expect(publicSwitch).not.toBeChecked();
      }

      const startSwitch = screen.getByRole("switch", { name: "Start when full" });
      expect(startSwitch).toBeChecked();
      startSwitch.focus();
      await user.keyboard(" ");
      expect(startSwitch).not.toBeChecked();

      const sandboxSwitch = screen.getByRole("switch", { name: "Sandbox Mode — allow debug actions" });
      await user.tab();
      expect(sandboxSwitch).toHaveFocus();
      expect(sandboxSwitch).not.toBeChecked();
      await user.keyboard("{Enter}");
      expect(sandboxSwitch).toBeChecked();

      const passwordSwitch = screen.getByRole("switch", { name: "Set password" });
      await user.tab();
      expect(passwordSwitch).toHaveFocus();
      expect(passwordSwitch).not.toBeChecked();
      await user.keyboard(" ");
      expect(passwordSwitch).toBeChecked();
      await user.type(screen.getByPlaceholderText("Game password"), "test-password");

      expect(onHost).not.toHaveBeenCalled();
      await user.click(screen.getByRole("button", {
        name: connectionMode === "server" ? "Host Game" : "Host P2P Game",
      }));
      expect(onHost).toHaveBeenCalledTimes(1);
      expect(onHost).toHaveBeenCalledWith(expect.objectContaining({
        public: connectionMode === "p2p",
        startWhenFull: false,
        password: "test-password",
        formatConfig: expect.objectContaining({ allow_debug_actions: true }),
      }));
    });

    it("clears a password when its named switch is turned off", async () => {
      const user = userEvent.setup();
      const onHost = vi.fn();
      render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode={connectionMode} />);

      const passwordSwitch = screen.getByRole("switch", { name: "Set password" });
      await user.click(passwordSwitch);
      await user.type(screen.getByPlaceholderText("Game password"), "discarded-password");
      await user.click(passwordSwitch);
      expect(screen.queryByPlaceholderText("Game password")).not.toBeInTheDocument();
      await user.click(passwordSwitch);
      expect(screen.getByPlaceholderText("Game password")).toHaveValue("");
      await user.click(passwordSwitch);

      expect(onHost).not.toHaveBeenCalled();
      await user.click(screen.getByRole("button", {
        name: connectionMode === "server" ? "Host Game" : "Host P2P Game",
      }));
      expect(onHost).toHaveBeenCalledTimes(1);
      expect(onHost).toHaveBeenCalledWith(expect.objectContaining({
        public: true,
        startWhenFull: true,
        password: "",
        formatConfig: expect.objectContaining({ allow_debug_actions: false }),
      }));
    });
  });

  it("updates switch names and sandbox help when the active locale changes", async () => {
    const user = userEvent.setup();
    i18n.addResourceBundle("de", "multiplayer", deMultiplayer, true, true);
    render(<HostSetup onHost={vi.fn()} onBack={vi.fn()} connectionMode="server" />);

    const translations = [
      ["List in lobby", "In Lobby listen"],
      ["Start when full", "Starten, wenn voll"],
      ["Sandbox Mode — allow debug actions", "Sandbox-Modus — Debug-Aktionen erlauben"],
      ["Set password", "Passwort festlegen"],
    ] as const;
    const switches = translations.map(([name]) => screen.getByRole("switch", { name }));
    const startSwitch = screen.getByRole("switch", { name: "Start when full" });
    const sandboxSwitch = screen.getByRole("switch", { name: "Sandbox Mode — allow debug actions" });
    expect(sandboxSwitch).toHaveAccessibleDescription(enMultiplayer.hostSetup.sandboxModeHelp);
    await user.click(startSwitch);

    // Component tests use their own lean i18next setup, without the production
    // preferences-store subscription. Change that test instance while mounted.
    await act(async () => {
      await i18n.changeLanguage("de");
    });

    for (const [index, [, name]] of translations.entries()) {
      expect(screen.getByRole("switch", { name })).toBe(switches[index]);
    }
    expect(sandboxSwitch).toHaveAccessibleDescription(deMultiplayer.hostSetup.sandboxModeHelp);
    expect(startSwitch).not.toBeChecked();
  });

  it("keeps sandbox descriptions associated with their own mounted form", () => {
    const first = render(<HostSetup onHost={vi.fn()} onBack={vi.fn()} connectionMode="server" />);
    const second = render(<HostSetup onHost={vi.fn()} onBack={vi.fn()} connectionMode="p2p" />);

    const descriptionIds = [first, second].map(({ container }) => {
      const control = within(container).getByRole("switch", { name: "Sandbox Mode — allow debug actions" });
      const description = within(container).getByText(enMultiplayer.hostSetup.sandboxModeHelp);
      expect(description.id).not.toBe("");
      expect(control).toHaveAttribute("aria-describedby", description.id);
      expect(control).toHaveAccessibleDescription(enMultiplayer.hostSetup.sandboxModeHelp);
      return description.id;
    });
    expect(new Set(descriptionIds).size).toBe(2);
  });

  it("allows Free-for-All hosts to choose 40-card deck size", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Free-for-All" }));
    await user.click(screen.getByRole("button", { name: "40" }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({
          format: "FreeForAll",
          deck_size: { type: "Minimum", data: 40 },
        }),
      }),
    );
  });

  it("submits interactive loop detection when selected", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Interactive" }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({ loopDetection: { type: "Interactive" } }),
    );
  });

  it("submits Two-Headed Giant as a four-seat human-only team format", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Two-Headed Giant" }));

    expect(screen.queryByRole("button", { name: "Human" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "AI difficulty" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({
          format: "TwoHeadedGiant",
          team_based: true,
          min_players: 4,
          max_players: 4,
        }),
        aiSeats: [],
      }),
    );
  });

  it("ignores stale restored AI seats for Two-Headed Giant", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();
    useMultiplayerStore.setState({
      lastHostConfig: {
        format: "TwoHeadedGiant",
        formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
        savedCustomFormatId: null,
        playerCount: 4,
        matchType: "Bo1",
        loopDetection: { type: "Off" },
        isPublic: true,
        startWhenFull: true,
        ranked: false,
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      },
    });

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    expect(screen.queryByRole("button", { name: "Human" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "AI difficulty" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({ format: "TwoHeadedGiant" }),
        aiSeats: [],
      }),
    );
    expect(useMultiplayerStore.getState().lastHostConfig?.aiSeats).toEqual([]);
  });

  it("renders and updates AI seat difficulty with translated labels", async () => {
    const user = userEvent.setup();

    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Human" }));

    const difficultyButton = screen.getByRole("button", { name: "AI difficulty" });
    expect(difficultyButton).toHaveTextContent("Medium");
    expect(screen.queryByText("VeryHard")).not.toBeInTheDocument();

    await user.click(difficultyButton);
    await user.click(screen.getByRole("option", { name: "Very Hard" }));

    expect(difficultyButton).toHaveTextContent("Very Hard");
    expect(screen.queryByText("VeryHard")).not.toBeInTheDocument();
  });

  it("hosts with the starting life the user typed after clearing the field", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode="server" />);

    // Commander's 40 is already in the box, so entering a non-standard value
    // means emptying it first. A per-keystroke clamp used to refill the box
    // with a fallback, and the typed digits landed after it (25 became 125).
    const life = screen.getByLabelText("Starting Life");
    await user.clear(life);
    await user.type(life, "25");

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({ starting_life: 25 }),
      }),
    );
  });

  it("keeps the last valid starting life when the field is left empty", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode="server" />);

    const life = screen.getByLabelText("Starting Life");
    await user.clear(life);

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({
          starting_life: FORMAT_DEFAULTS.Commander.starting_life,
        }),
      }),
    );
  });

  /**
   * `FORMAT_DEFAULTS` is built from the BUILT-IN registry and has no entry for
   * any `Custom:<id>` key. The seat-ceiling check used to index it directly
   * with the remembered format, so `FORMAT_DEFAULTS["Custom:0"]` was
   * `undefined` and reading `.min_players` off it threw — a hard crash on
   * mount for anyone whose last hosted game used a custom format.
   *
   * Rendering IS the assertion: revert the `isKnownFormat` guard and this test
   * throws "Cannot read properties of undefined (reading 'min_players')"
   * before any query runs. Both connection modes are exercised because only
   * the P2P branch performs the lookup.
   */
  describe.each(["p2p", "server"] as const)(
    "with a remembered custom format (%s mode)",
    (connectionMode) => {
      const customFormatConfig = {
        format: "Custom:0" as const,
        starting_life: 20,
        min_players: 2,
        max_players: 4,
        deck_size: { type: "Minimum" as const, data: 60 },
        singleton: false,
        command_zone: false,
        commander_damage_threshold: null,
        range_of_influence: null,
        team_based: false,
        uses_commander: false,
        supplies_fixed_deck: false,
        sideboard_policy: { type: "Limited" as const, data: 15 },
        default_deck_copy_limit: { type: "UpTo" as const, data: 4 },
        allow_debug_actions: false,
      };

      it("mounts and seeds the form from the format's own resolved config", () => {
        useMultiplayerStore.setState({
          lastHostConfig: {
            format: "Custom:0",
            formatConfig: customFormatConfig,
            savedCustomFormatId: "saved-1",
            playerCount: 3,
            matchType: "Bo1",
            loopDetection: { type: "Off" },
            isPublic: true,
            startWhenFull: true,
            ranked: false,
            aiSeats: [],
          },
        });

        render(
          <HostSetup onHost={vi.fn()} onBack={vi.fn()} connectionMode={connectionMode} />,
        );

        // Reached the rendered form at all — i.e. the seat-ceiling lookup did
        // not throw — and read the custom format's own starting life rather
        // than a registry default that does not exist for it.
        expect(screen.getByLabelText("Starting Life")).toHaveValue(20);
      });
    },
  );

  /**
   * No `CustomFormatRules` deck-validation resolver exists yet (Phase 1d) —
   * `validate_deck_for_format` (the authoritative game-creation gate)
   * unconditionally rejects every Custom-format deck. Before this test, a
   * host could select a saved custom format, fill in a deck, and click Host
   * Game, only to have that submission deterministically fail at engine
   * init with no warning beforehand. The Host action must be unavailable for
   * that selection instead of walking the user into a guaranteed dead end.
   */
  it("disables the Host action once a saved custom format is selected", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    const saved = saveCustomFormat("Grandpa's House Rules", {
      rules: {
        id: 0,
        structural: {
          starting_life: 20,
          min_players: 2,
          max_players: 4,
          deck_size: { type: "Minimum", data: 60 },
          singleton: false,
          command_zone_mode: "Disabled",
          range_of_influence: null,
          team_based: false,
          sideboard_policy: { type: "Limited", data: 15 },
          default_deck_copy_limit: { type: "UpTo", data: 4 },
        },
        legality: {
          legal_sets: null,
          banned: [],
          restricted: [],
          legacy: {
            mana_burn: "Modern",
            damage_timing: "Modern",
            wish_scope: "PostM10SideboardOnly",
            legend_rule_scope: "Modern",
          },
        },
      },
      label: "Grandpa's House Rules",
      short_label: "GRA",
      description: "60-card minimum, 2–4 players",
      reprint_policy: null,
      printing_fidelity: "NotApplicable",
    });

    render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode="server" />);

    expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Use" }));

    const hostButton = await screen.findByRole("button", { name: "Host Game" });
    expect(hostButton).toBeDisabled();
    expect(
      screen.getByText(/Hosting with a custom format isn't supported yet/),
    ).toBeInTheDocument();

    await user.click(hostButton);
    expect(onHost).not.toHaveBeenCalled();

    const selectedConfig = useMultiplayerStore.getState().formatConfig;
    if (selectedConfig == null) throw new Error("saved custom format did not resolve");
    useMultiplayerStore.getState().rememberHostConfig({
      format: selectedConfig.format,
      formatConfig: selectedConfig,
      savedCustomFormatId: saved.id,
      playerCount: 2,
      matchType: "Bo1",
      loopDetection: { type: "Off" },
      isPublic: true,
      startWhenFull: true,
      ranked: false,
      aiSeats: [],
    });

    await user.click(screen.getByRole("button", { name: "Delete" }));

    expect(useMultiplayerStore.getState().lastHostConfig).toBeNull();
    expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled();
  });

  it("hides saved formats whose minimum exceeds the P2P ceiling", () => {
    const saved = saveCustomFormat("Eight-seat format", {
      rules: {
        id: 0,
        structural: {
          starting_life: 20,
          min_players: 7,
          max_players: 8,
          deck_size: { type: "Minimum", data: 60 },
          singleton: false,
          command_zone_mode: "Disabled",
          range_of_influence: null,
          team_based: false,
          sideboard_policy: { type: "Limited", data: 15 },
          default_deck_copy_limit: { type: "UpTo", data: 4 },
        },
        legality: {
          legal_sets: null,
          banned: [],
          restricted: [],
          legacy: {
            mana_burn: "Modern",
            damage_timing: "Modern",
            wish_scope: "PostM10SideboardOnly",
            legend_rule_scope: "Modern",
          },
        },
      },
      label: "Eight-seat format",
      short_label: "EIG",
      description: "Seven to eight players",
      reprint_policy: null,
      printing_fidelity: "NotApplicable",
    });

    render(<HostSetup onHost={vi.fn()} onBack={vi.fn()} connectionMode="p2p" />);

    expect(screen.queryByText(saved.name)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use" })).not.toBeInTheDocument();
  });
});
