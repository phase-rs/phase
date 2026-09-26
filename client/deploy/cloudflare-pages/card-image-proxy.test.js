import assert from "node:assert/strict";
import test from "node:test";

import { proxyCardImage } from "./card-image-proxy.js";

test("rejects methods that cannot retrieve an image", async () => {
  const response = await proxyCardImage(
    {
      request: new Request("https://phase.example/card-image-art/example.jpg", {
        method: "POST",
      }),
      params: { path: ["example.jpg"] },
    },
    "https://cards.scryfall.io",
  );

  assert.equal(response.status, 405);
  assert.equal(response.headers.get("allow"), "GET, HEAD");
});

test("forwards only a validated path to the fixed upstream", async () => {
  const originalFetch = globalThis.fetch;
  let forwardedUrl;
  globalThis.fetch = async (url) => {
    forwardedUrl = String(url);
    return new Response("image", {
      headers: {
        "content-type": "image/jpeg",
        "set-cookie": "not-for-the-client=true",
      },
    });
  };

  try {
    const response = await proxyCardImage(
      {
        request: new Request(
          "https://phase.example/card-image-art/art_crop/a%20b.jpg?version=1",
        ),
        params: { path: ["art_crop", "a b.jpg"] },
      },
      "https://cards.scryfall.io",
    );

    assert.equal(
      forwardedUrl,
      "https://cards.scryfall.io/art_crop/a%20b.jpg?version=1",
    );
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("set-cookie"), null);
    assert.equal(
      response.headers.get("cache-control"),
      "public, max-age=31536000, immutable",
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("rejects path traversal segments", async () => {
  const response = await proxyCardImage(
    {
      request: new Request("https://phase.example/card-image-art/../secret"),
      params: { path: ["..", "secret"] },
    },
    "https://cards.scryfall.io",
  );

  assert.equal(response.status, 400);
});
