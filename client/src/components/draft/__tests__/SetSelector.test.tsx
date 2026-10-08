import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { SetSelector } from "../SetSelector";

afterEach(() => {
  cleanup();
  // The scroll-stability test spies on a prototype and stubs a global; both
  // would leak into later tests if a failing assertion skipped its own cleanup.
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

/**
 * `SetSelector` loads its set list from the draft-pools and Scryfall data
 * files. Both are large gitignored artifacts, so the tests serve a two-set
 * stand-in and assert on the pack list the component builds from it.
 */
const POOLS = {
  isd: { name: "Innistrad" },
  dka: { name: "Dark Ascension" },
};

const SCRYFALL_SETS = {
  isd: { name: "Innistrad", icon_svg_uri: "", released_at: "2011-09-30" },
  dka: { name: "Dark Ascension", icon_svg_uri: "", released_at: "2012-02-03" },
};

function mockDataFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) =>
      ({
        ok: true,
        json: async () => (url.includes("scryfall") ? SCRYFALL_SETS : POOLS),
      }) as unknown as Response,
    ),
  );
}

/** The pack list, once the set grid has loaded. */
async function packEntries(): Promise<string[]> {
  const list = await screen.findByRole("list");
  return within(list)
    .getAllByRole("listitem")
    .map((item) => item.textContent ?? "");
}

async function addPack(setName: string) {
  await userEvent.click(
    await screen.findByRole("button", { name: `Add a pack of ${setName}` }),
  );
}

describe("SetSelector", () => {
  beforeEach(() => {
    mockDataFetch();
  });

  it("defaults the pack order to the order the sets were picked", async () => {
    const onStartDraft = vi.fn();
    render(<SetSelector onStartDraft={onStartDraft} defaultPackCount={3} />);

    await addPack("Dark Ascension");
    await addPack("Innistrad");

    const entries = await packEntries();
    expect(entries[0]).toContain("Dark Ascension");
    expect(entries[1]).toContain("Innistrad");

    await userEvent.click(screen.getByRole("button", { name: "Start Draft" }));
    expect(onStartDraft).toHaveBeenCalledWith([
      { code: "DKA", name: "Dark Ascension" },
      { code: "ISD", name: "Innistrad" },
    ]);
  });

  it("lets the same set fill more than one pack", async () => {
    const onStartDraft = vi.fn();
    render(<SetSelector onStartDraft={onStartDraft} defaultPackCount={3} />);

    await addPack("Innistrad");
    await addPack("Dark Ascension");
    await addPack("Innistrad");

    await userEvent.click(screen.getByRole("button", { name: "Start Draft" }));
    expect(onStartDraft).toHaveBeenCalledWith([
      { code: "ISD", name: "Innistrad" },
      { code: "DKA", name: "Dark Ascension" },
      { code: "ISD", name: "Innistrad" },
    ]);
  });

  it("lets the picked order be overridden", async () => {
    const onStartDraft = vi.fn();
    render(<SetSelector onStartDraft={onStartDraft} defaultPackCount={3} />);

    await addPack("Innistrad");
    await addPack("Dark Ascension");
    await userEvent.click(screen.getByRole("button", { name: "Move pack 2 earlier" }));

    const entries = await packEntries();
    expect(entries[0]).toContain("Dark Ascension");
    expect(entries[1]).toContain("Innistrad");
  });

  it("removes a pack without disturbing the rest of the order", async () => {
    render(<SetSelector onStartDraft={vi.fn()} defaultPackCount={3} />);

    await addPack("Innistrad");
    await addPack("Dark Ascension");
    await addPack("Innistrad");
    await userEvent.click(screen.getByRole("button", { name: "Remove pack 2" }));

    const entries = await packEntries();
    expect(entries).toHaveLength(2);
    expect(entries[0]).toContain("Innistrad");
    expect(entries[1]).toContain("Innistrad");
  });

  it("repeats the last chosen set to fill the event's default pack count", async () => {
    const onStartDraft = vi.fn();
    render(<SetSelector onStartDraft={onStartDraft} defaultPackCount={3} />);

    await addPack("Innistrad");
    await userEvent.click(
      screen.getByRole("button", { name: "Fill the remaining 2 packs with Innistrad" }),
    );

    await userEvent.click(screen.getByRole("button", { name: "Start Draft" }));
    expect(onStartDraft).toHaveBeenCalledWith([
      { code: "ISD", name: "Innistrad" },
      { code: "ISD", name: "Innistrad" },
      { code: "ISD", name: "Innistrad" },
    ]);
  });

  it("offers to fill only the packs a mixed selection has left over", async () => {
    render(<SetSelector onStartDraft={vi.fn()} defaultPackCount={3} />);

    await addPack("Innistrad");
    await addPack("Dark Ascension");
    await userEvent.click(
      screen.getByRole("button", { name: "Fill the last pack with Dark Ascension" }),
    );

    const entries = await packEntries();
    expect(entries.map((e) => e.replace(/\s+/g, " "))).toHaveLength(3);
    expect(entries[2]).toContain("Dark Ascension");
    // The event is at its default count, so nothing is left to fill.
    expect(screen.queryByRole("button", { name: /Fill the/ })).not.toBeInTheDocument();
  });

  it("holds a fixed-length event to exactly its pack count", async () => {
    render(
      <SetSelector onStartDraft={vi.fn()} defaultPackCount={2} fixedPackCount />,
    );

    await addPack("Innistrad");
    // One short of the required two: the event cannot start yet.
    expect(screen.getByRole("button", { name: "Start Draft" })).toBeDisabled();

    await addPack("Dark Ascension");
    expect(screen.getByRole("button", { name: "Start Draft" })).toBeEnabled();
    // Full: the grid stops accepting packs rather than overfilling the event.
    expect(screen.getByRole("button", { name: "Add a pack of Innistrad" })).toBeDisabled();
  });

  it("holds a variable-length event to its own pack count", async () => {
    // A Quick Draft's list is not fixed-length — one set is a legal selection,
    // because a short sequence repeats its last entry to fill every booster.
    // The event's booster count is still the ceiling: naming a fourth pack
    // builds a selection the engine refuses (`ResolvedSetSelection`), so the
    // grid must stop at three rather than offering a start that cannot run.
    const onStartDraft = vi.fn();
    render(<SetSelector onStartDraft={onStartDraft} defaultPackCount={3} />);

    await addPack("Innistrad");
    // One set is already a startable draft, unlike a fixed-length event.
    expect(screen.getByRole("button", { name: "Start Draft" })).toBeEnabled();

    await addPack("Dark Ascension");
    await addPack("Innistrad");
    expect(await packEntries()).toHaveLength(3);
    expect(screen.getByRole("button", { name: "Add a pack of Innistrad" })).toBeDisabled();

    await userEvent.click(screen.getByRole("button", { name: "Start Draft" }));
    expect(onStartDraft).toHaveBeenCalledWith([
      { code: "ISD", name: "Innistrad" },
      { code: "DKA", name: "Dark Ascension" },
      { code: "ISD", name: "Innistrad" },
    ]);
  });

  it("collects distinct, unbounded candidate sets for a Chaos host", async () => {
    const onStartDraft = vi.fn();
    render(<SetSelector onStartDraft={onStartDraft} defaultPackCount={1} candidatePool />);

    await userEvent.click(await screen.findByRole("button", { name: "Add Innistrad as a candidate" }));
    await userEvent.click(screen.getByRole("button", { name: "Add Dark Ascension as a candidate" }));
    // A candidate list is not a pack order: the same set cannot bias the
    // host's random assignment just because it was clicked twice.
    await userEvent.click(screen.getByRole("button", { name: "Add Innistrad as a candidate" }));

    expect(await packEntries()).toHaveLength(2);
    expect(screen.queryByRole("button", { name: /Move pack/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Fill the/ })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Start Draft" }));
    expect(onStartDraft).toHaveBeenCalledWith([
      { code: "ISD", name: "Innistrad" },
      { code: "DKA", name: "Dark Ascension" },
    ]);
  });

  it("trims a flexible selection when the event pack count decreases", async () => {
    const onStartDraft = vi.fn();
    const { rerender } = render(
      <SetSelector onStartDraft={onStartDraft} defaultPackCount={3} />,
    );

    await addPack("Innistrad");
    await addPack("Dark Ascension");
    await addPack("Innistrad");

    rerender(<SetSelector onStartDraft={onStartDraft} defaultPackCount={2} />);

    await waitFor(async () => expect(await packEntries()).toHaveLength(2));
    await userEvent.click(screen.getByRole("button", { name: "Start Draft" }));
    expect(onStartDraft).toHaveBeenCalledWith([
      { code: "ISD", name: "Innistrad" },
      { code: "DKA", name: "Dark Ascension" },
    ]);
  });

  it("holds the set grid still when the pack list resizes above it", async () => {
    // The grid sits below the pack list, so a list that grows by N pixels
    // slides the tile under the pointer down by N unless the scroll follows.
    const scrollBy = vi.fn();
    vi.stubGlobal("scrollBy", scrollBy);
    let listHeight = 40;
    let listTop = -500; // scrolled past: the growth is above the fold
    vi.spyOn(HTMLDivElement.prototype, "getBoundingClientRect").mockImplementation(
      () => ({ height: listHeight, top: listTop }) as DOMRect,
    );

    render(<SetSelector onStartDraft={vi.fn()} defaultPackCount={3} />);
    await addPack("Innistrad");

    listHeight = 96;
    await addPack("Dark Ascension");
    expect(scrollBy).toHaveBeenCalledWith({ top: 56, behavior: "instant" });

    // A list the player can see growing must not drag them off the top.
    scrollBy.mockClear();
    listTop = 0;
    listHeight = 140;
    await addPack("Innistrad");
    expect(scrollBy).not.toHaveBeenCalled();
  });

  it("starts immediately from one set when the caller can only carry one", async () => {
    const onStartDraft = vi.fn();
    render(
      <SetSelector onStartDraft={onStartDraft} defaultPackCount={3} singleSet />,
    );

    await userEvent.click(await screen.findByRole("button", { name: "Draft Innistrad" }));

    await waitFor(() =>
      expect(onStartDraft).toHaveBeenCalledWith([{ code: "ISD", name: "Innistrad" }]),
    );
    // No pack list and no separate start step in this mode.
    expect(screen.queryByRole("list")).not.toBeInTheDocument();
  });
});
