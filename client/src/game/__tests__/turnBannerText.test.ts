import { afterEach, describe, expect, it } from "vitest";

import i18n from "../../i18n";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { turnBannerText } from "../dispatch";

/**
 * The turn banner routes its whole template through i18n, so the localized
 * fallback opponent name ("対戦相手2") never lands inside an English
 * "…'S TURN" sentence.
 */
describe("turnBannerText", () => {
  afterEach(async () => {
    useMultiplayerStore.setState({ playerNames: new Map() });
    await i18n.changeLanguage("en");
  });

  it("renders English banners upper-cased", async () => {
    await i18n.changeLanguage("en");
    useMultiplayerStore.setState({ playerNames: new Map() });
    expect(turnBannerText(0, true)).toBe("YOUR TURN");
    expect(turnBannerText(1, false)).toBe("OPP 2'S TURN");
  });

  it("localizes the whole banner, not just the fallback name", async () => {
    await i18n.changeLanguage("ja");
    useMultiplayerStore.setState({ playerNames: new Map() });
    expect(turnBannerText(0, true)).toBe("あなたのターン");
    expect(turnBannerText(1, false)).toBe("対戦相手2のターン");
  });

  it("interpolates a recorded custom player name into the localized template", async () => {
    await i18n.changeLanguage("ja");
    useMultiplayerStore.setState({ playerNames: new Map([[1, "Alice"]]) });
    expect(turnBannerText(1, false)).toBe("ALICEのターン");
  });
});
