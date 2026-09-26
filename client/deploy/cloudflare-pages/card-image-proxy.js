const FORWARDED_REQUEST_HEADERS = [
  "accept",
  "if-modified-since",
  "if-none-match",
  "range",
];

export async function proxyCardImage(context, upstreamOrigin) {
  const { request } = context;
  if (request.method !== "GET" && request.method !== "HEAD") {
    return new Response("Method not allowed", {
      status: 405,
      headers: { Allow: "GET, HEAD" },
    });
  }

  const rawPath = context.params.path;
  const segments = Array.isArray(rawPath) ? rawPath : [rawPath];
  if (
    segments.length === 0 ||
    segments.some(
      (segment) =>
        typeof segment !== "string" ||
        segment.length === 0 ||
        segment === "." ||
        segment === "..",
    )
  ) {
    return new Response("Invalid image path", { status: 400 });
  }

  const incomingUrl = new URL(request.url);
  const upstreamUrl = new URL(
    `/${segments.map((segment) => encodeURIComponent(segment)).join("/")}`,
    upstreamOrigin,
  );
  upstreamUrl.search = incomingUrl.search;

  const requestHeaders = new Headers();
  for (const name of FORWARDED_REQUEST_HEADERS) {
    const value = request.headers.get(name);
    if (value !== null) requestHeaders.set(name, value);
  }

  try {
    const upstream = await fetch(upstreamUrl, {
      method: request.method,
      headers: requestHeaders,
      redirect: "follow",
    });
    const responseHeaders = new Headers(upstream.headers);
    responseHeaders.delete("set-cookie");
    responseHeaders.set(
      "Cache-Control",
      upstream.status < 400
        ? "public, max-age=31536000, immutable"
        : "no-store",
    );
    responseHeaders.set("X-Content-Type-Options", "nosniff");

    return new Response(upstream.body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: responseHeaders,
    });
  } catch {
    return new Response("Image upstream unavailable", { status: 502 });
  }
}
