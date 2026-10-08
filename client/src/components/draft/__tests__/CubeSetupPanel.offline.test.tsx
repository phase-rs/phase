import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CUBE_IMPORT_ERROR_KEYS } from "../../../services/cubeCobra";
import { useConnectivityStore } from "../../../stores/connectivityStore";
import { CubeSetupPanel, DEFAULT_CUBE_SETTINGS } from "../CubeSetupPanel";

const mocks = vi.hoisted(() => ({
  fetchCubeList: vi.fn(),
}));

vi.mock("../../../services/cubeCobra", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../../services/cubeCobra")>(),
  fetchCubeList: mocks.fetchCubeList,
}));

describe("CubeSetupPanel offline URL loading", () => {
  beforeEach(() => {
    mocks.fetchCubeList.mockReset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it("reactively disables URL loading offline while leaving the URL editable", async () => {
    const user = userEvent.setup();
    render(<CubeSetupPanel onStart={vi.fn()} />);

    const urlInput = screen.getByPlaceholderText(/raw export URL/i);
    const loadButton = screen.getByRole("button", { name: "Load URL" });
    await user.type(urlInput, "https://cubecobra.com/cube/list/abc123");
    expect(loadButton).toBeEnabled();

    act(() => useConnectivityStore.getState().setForcedOffline(true));

    expect(urlInput).toBeEnabled();
    expect(loadButton).toBeDisabled();
    expect(screen.getByText(/Loading cube lists from URLs is unavailable offline/i)).toBeInTheDocument();
    await user.click(loadButton);
    expect(mocks.fetchCubeList).not.toHaveBeenCalled();
  });

  it("keeps a manually pasted cube list startable offline", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    useConnectivityStore.getState().setForcedOffline(true);
    render(<CubeSetupPanel onStart={onStart} />);

    await user.type(screen.getByPlaceholderText(/Lightning Bolt/i), "1 Lightning Bolt");
    await user.click(screen.getByRole("button", { name: "Start Cube Draft" }));

    await waitFor(() => expect(onStart).toHaveBeenCalledOnce());
    expect(onStart).toHaveBeenCalledWith({
      cubeName: "Custom Cube",
      cubeListText: "1 Lightning Bolt",
      settings: DEFAULT_CUBE_SETTINGS,
    });
  });

  it("keeps a cube list loaded online startable after transitioning offline", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    mocks.fetchCubeList.mockResolvedValue("1 Black Lotus");
    render(<CubeSetupPanel onStart={onStart} />);

    await user.type(screen.getByPlaceholderText(/raw export URL/i), "https://example.com/cube.txt");
    await user.click(screen.getByRole("button", { name: "Load URL" }));
    await waitFor(() => expect(screen.getByPlaceholderText(/Lightning Bolt/i)).toHaveValue("1 Black Lotus"));

    act(() => useConnectivityStore.getState().setForcedOffline(true));

    await user.click(screen.getByRole("button", { name: "Start Cube Draft" }));
    await waitFor(() => expect(onStart).toHaveBeenCalledWith(expect.objectContaining({
      cubeListText: "1 Black Lotus",
    })));
  });

  it("translates the offline service backstop while preserving ordinary service text", async () => {
    const user = userEvent.setup();
    mocks.fetchCubeList.mockRejectedValueOnce(new Error(CUBE_IMPORT_ERROR_KEYS.offline));
    render(<CubeSetupPanel onStart={vi.fn()} />);

    await user.type(screen.getByPlaceholderText(/raw export URL/i), "https://example.com/cube.txt");
    await user.click(screen.getByRole("button", { name: "Load URL" }));
    expect(await screen.findByText(/Loading cube lists from URLs is unavailable offline/i)).toBeInTheDocument();

    mocks.fetchCubeList.mockRejectedValueOnce(new Error("Cube export failed"));
    await user.click(screen.getByRole("button", { name: "Load URL" }));
    expect(await screen.findByText("Cube export failed")).toBeInTheDocument();
  });
});
