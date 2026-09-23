#!/usr/bin/env node

const raw = process.argv[2] ?? "";

function fail(message) {
  console.error(`ERROR: ${message}`);
  process.exit(2);
}

if (!raw || raw.trim() !== raw) fail("origin must not be empty or contain surrounding whitespace");
if (raw.endsWith("/")) fail("origin must not have a trailing slash");

let parsed;
try {
  parsed = new URL(raw);
} catch {
  fail("origin must be a valid URL");
}

if (parsed.protocol !== "https:") fail("origin must use https");
if (!parsed.hostname || parsed.username || parsed.password) {
  fail("origin must contain only an https host and optional port");
}
if (parsed.pathname !== "/" || parsed.search || parsed.hash) {
  fail("origin must not contain a path, query, or fragment");
}

process.stdout.write(`${parsed.protocol}//${parsed.host}\n`);
