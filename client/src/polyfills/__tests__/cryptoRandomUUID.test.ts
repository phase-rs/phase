import { afterEach, describe, expect, it } from "vitest";

import {
  createRandomUUID,
  installCryptoRandomUUID,
} from "../cryptoRandomUUID";

const originalCryptoDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crypto");

function setCrypto(value: Partial<Crypto> | undefined): void {
  Object.defineProperty(globalThis, "crypto", {
    configurable: true,
    value,
  });
}

afterEach(() => {
  if (originalCryptoDescriptor) {
    Object.defineProperty(globalThis, "crypto", originalCryptoDescriptor);
  }
});

describe("crypto.randomUUID polyfill", () => {
  it("creates RFC 4122 version 4 UUIDs from Web Crypto bytes", () => {
    const uuid = createRandomUUID((array) => {
      array.set([
        0x00, 0x11, 0x22, 0x33,
        0x44, 0x55,
        0x66, 0x77,
        0x88, 0x99,
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
      ]);
      return array;
    });

    expect(uuid).toBe("00112233-4455-4677-8899-aabbccddeeff");
  });

  it("installs randomUUID when getRandomValues exists but randomUUID does not", () => {
    setCrypto({
      getRandomValues: ((array: Uint8Array) => {
        array.fill(0xab);
        return array;
      }) as unknown as Crypto["getRandomValues"],
    });

    installCryptoRandomUUID();

    expect(globalThis.crypto.randomUUID()).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
  });

  it("keeps the browser implementation when randomUUID already exists", () => {
    const randomUUID = () => "native-random-uuid";
    setCrypto({
      getRandomValues: ((array: Uint8Array) => array) as unknown as Crypto["getRandomValues"],
      randomUUID: randomUUID as Crypto["randomUUID"],
    });

    installCryptoRandomUUID();

    expect(globalThis.crypto.randomUUID).toBe(randomUUID);
  });
});
