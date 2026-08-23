import { describe, expect, it } from "vitest";
import { faceDownMarkerRef } from "../faceDownMarker.ts";

describe("faceDownMarkerRef", () => {
  it("maps each rules cause onto the printing paper play uses", () => {
    expect(faceDownMarkerRef(true, "Manifest")?.face_name).toBe("manifest");
    expect(faceDownMarkerRef(true, "Morph")?.face_name).toBe("morph");
    // Cloak (CR 701.58a) and disguise (CR 702.168a) are different rules that
    // share one printed token — the mapping is where they converge, not the
    // engine's enum.
    expect(faceDownMarkerRef(true, "Cloak")?.face_name).toBe("a mysterious creature");
    expect(faceDownMarkerRef(true, "Disguise")?.face_name).toBe("a mysterious creature");
    expect(faceDownMarkerRef(true, "Cloak")?.scryfall_oracle_id).toBe(
      faceDownMarkerRef(true, "Disguise")?.scryfall_oracle_id,
    );
  });

  it("has no marker for a cause with no printed token", () => {
    // Ixidron turns permanents face down with no keyword action, and Wizards
    // prints nothing for it — the generic card back stays.
    expect(faceDownMarkerRef(true, "TurnedFaceDown")).toBeNull();
  });

  it("stays null unless the permanent is actually face down", () => {
    // The engine leaves the cause on the object after it turns face up, so
    // every reader must gate on `face_down`. This is that gate.
    expect(faceDownMarkerRef(false, "Manifest")).toBeNull();
    expect(faceDownMarkerRef(true, null)).toBeNull();
    expect(faceDownMarkerRef(true, undefined)).toBeNull();
  });
});
