import { describe, expect, it } from "vitest";
import { cardImageLookup } from "../cardImageLookup.ts";

describe("cardImageLookup", () => {
  it("returns front-face lookup for a plain (non-transformed) card", () => {
    expect(
      cardImageLookup({
        name: "Lightning Bolt",
        transformed: false,
        back_face: null,
      }),
    ).toEqual({ name: "Lightning Bolt", faceIndex: 0 });
  });

  it("returns front-face lookup for an untransformed DFC (back_face present but not flipped)", () => {
    expect(
      cardImageLookup({
        name: "The Legend of Kuruk",
        transformed: false,
        back_face: {
          name: "Kuruk, the Mastodon",
        } as never,
      }),
    ).toEqual({ name: "The Legend of Kuruk", faceIndex: 0 });
  });

  it("resolves a transformed permanent to the stashed front-face name + faceIndex 1", () => {
    // After transform, the engine swaps obj.name to the back-face name and
    // stashes the original front-face characteristics in obj.back_face. The
    // Scryfall data map indexes only the front-face name, so the lookup must
    // use obj.back_face.name (which holds the front name) to hit the entry.
    expect(
      cardImageLookup({
        name: "Kuruk, the Mastodon",
        transformed: true,
        back_face: {
          name: "The Legend of Kuruk",
        } as never,
      }),
    ).toEqual({ name: "The Legend of Kuruk", faceIndex: 1 });
  });

  it("falls back to obj.name when transformed but back_face is missing", () => {
    expect(
      cardImageLookup({
        name: "Kuruk, the Mastodon",
        transformed: true,
        back_face: null,
      }),
    ).toEqual({ name: "Kuruk, the Mastodon", faceIndex: 1 });
  });

  it("returns oracle_id + face name when printed_ref is present (single-faced)", () => {
    expect(
      cardImageLookup({
        name: "Sol Ring",
        transformed: false,
        back_face: null,
        printed_ref: { oracle_id: "abc-123", face_name: "Sol Ring" },
      }),
    ).toEqual({
      oracleId: "abc-123",
      faceName: "Sol Ring",
      name: "Sol Ring",
      faceIndex: 0,
    });
  });

  it("returns the played-face oracle_id for an MDFC played as Scryfall's back face", () => {
    // Pinnacle Monk // Mystic Peak: Scryfall front face is Pinnacle Monk;
    // when the player casts the land face, the engine carries Mystic Peak as
    // the primary identity. Both faces share the same oracle_id, and the
    // face name disambiguates which Scryfall card_faces entry to render.
    expect(
      cardImageLookup({
        name: "Mystic Peak",
        transformed: false,
        back_face: {
          name: "Pinnacle Monk",
          printed_ref: { oracle_id: "f3d48efa", face_name: "Pinnacle Monk" },
        } as never,
        printed_ref: { oracle_id: "f3d48efa", face_name: "Mystic Peak" },
      }),
    ).toEqual({
      oracleId: "f3d48efa",
      faceName: "Mystic Peak",
      name: "Mystic Peak",
      faceIndex: 0,
    });
  });

  it("uses obj.printed_ref directly when transformed (engine tracks current face)", () => {
    // After transform, the engine overwrites obj.printed_ref to point at the
    // back face (printed_cards.rs:190). The Scryfall service resolves the
    // correct card_faces entry from face_name, so we don't swap here.
    expect(
      cardImageLookup({
        name: "Kuruk, the Mastodon",
        transformed: true,
        back_face: {
          name: "The Legend of Kuruk",
          printed_ref: { oracle_id: "k-front", face_name: "The Legend of Kuruk" },
        } as never,
        printed_ref: { oracle_id: "k-front", face_name: "Kuruk, the Mastodon" },
      }),
    ).toEqual({
      oracleId: "k-front",
      faceName: "Kuruk, the Mastodon",
      name: "Kuruk, the Mastodon",
      faceIndex: 1,
    });
  });
});
