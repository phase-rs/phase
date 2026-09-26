import { beforeEach, describe, expect, it } from "vitest";

import { claimServiceWorkerReload, hasServiceWorkerReloadBudget } from "../updateMarker";

describe("service worker reload budget", () => {
  beforeEach(() => {
    sessionStorage.clear();
  });

  it("reads the budget without consuming it", () => {
    expect(hasServiceWorkerReloadBudget()).toBe(true);
    expect(hasServiceWorkerReloadBudget()).toBe(true);
    expect(sessionStorage.getItem("phase:sw-reload-count")).toBeNull();

    expect(claimServiceWorkerReload()).toBe(true);
    expect(hasServiceWorkerReloadBudget()).toBe(false);
    expect(claimServiceWorkerReload()).toBe(false);
  });
});
