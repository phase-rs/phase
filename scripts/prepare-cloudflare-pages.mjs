#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  copyFile,
  lstat,
  mkdir,
  readFile,
  readdir,
  rm,
  stat,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createReadStream } from "node:fs";
import { createBrotliDecompress } from "node:zlib";
import { Transform, Writable } from "node:stream";
import { pipeline } from "node:stream/promises";

export const PAGES_MAX_FILE_BYTES = 25 * 1024 * 1024;
export const PAGES_FREE_MAX_FILES = 20_000;

const MAX_ARTIFACT_PROVENANCE_JSON_BYTES = 512;
const ARTIFACT_PROVENANCE_FIELDS = new Set([
  "sourceRevision",
  "sourceArtifactSha256",
  "externalEngineWasmSha256",
  "buildRunId",
]);
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPOSITORY_ROOT = path.resolve(SCRIPT_DIR, "..");
const DATA_FILES_PATH = path.join(REPOSITORY_ROOT, "data-files.json");
const HEADERS_PATH = path.join(
  REPOSITORY_ROOT,
  "client",
  "deploy",
  "cloudflare-pages",
  "_headers",
);

const CARD_CORPUS_PATTERN = /^card-data-([0-9a-f]{16})\.json$/;
const ENGINE_WASM_PATTERN = /^engine_wasm_bg-([0-9a-f]{16})\.wasm$/;
const SECRET_LIKE_PATTERN =
  /(?:\bAIza[0-9a-z_-]{35}\b|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b|\bsk[-_](?:(?:live|test)[-_])?[a-z0-9_-]{16,}\b|\bgithub_pat_[a-z0-9_]{20,}\b|\bgh[pousr]_[a-z0-9]{20,}\b|\bxox[baprs]-[a-z0-9-]{10,}\b|\bglpat-[a-z0-9_-]{20,}\b|\beyj[a-z0-9_-]{16,}\.[a-z0-9_-]{8,}\.|(?:api[_-]?key|access[_-]?token|authorization|private[_-]?key|secret|password)\s*[:=])/i;

function fail(message) {
  throw new Error(message);
}

function compareStrings(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function toPosix(relativePath) {
  return relativePath.split(path.sep).join("/");
}

function isSameOrNested(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

async function requireDirectoryWithoutSymlinks(directoryPath, label) {
  const resolved = path.resolve(directoryPath);
  const root = path.parse(resolved).root;
  const segments = resolved.slice(root.length).split(path.sep).filter(Boolean);
  let current = root;

  for (const segment of segments) {
    current = path.join(current, segment);
    let entry;
    try {
      entry = await lstat(current);
    } catch (error) {
      if (error.code === "ENOENT") {
        fail(`${label} path does not exist: ${current}`);
      }
      throw error;
    }
    if (entry.isSymbolicLink()) fail(`${label} path contains a symlink: ${current}`);
    if (!entry.isDirectory()) fail(`${label} path component is not a directory: ${current}`);
  }

  const info = await lstat(resolved);
  if (!info.isDirectory() || info.isSymbolicLink()) fail(`${label} is not a real directory: ${resolved}`);
  return resolved;
}

async function assertOutputAbsentAndSafe(outputPath) {
  const parent = path.dirname(outputPath);
  await requireDirectoryWithoutSymlinks(parent, "output parent");
  try {
    await lstat(outputPath);
  } catch (error) {
    if (error.code === "ENOENT") return;
    throw error;
  }
  fail(`output destination already exists: ${outputPath}`);
}

async function collectTree(rootPath) {
  const files = [];

  async function visit(directoryPath, relativeDirectory) {
    const children = await readdir(directoryPath, { withFileTypes: true });
    children.sort((left, right) => compareStrings(left.name, right.name));

    for (const child of children) {
      if (
        child.name === "." ||
        child.name === ".." ||
        child.name.includes("/") ||
        child.name.includes("\\") ||
        /[\u0000-\u001f\u007f]/.test(child.name)
      ) {
        fail(`unsafe path component in build tree: ${child.name}`);
      }

      const absolutePath = path.join(directoryPath, child.name);
      const relativePath = relativeDirectory
        ? `${relativeDirectory}/${child.name}`
        : child.name;
      const info = await lstat(absolutePath);

      if (info.isSymbolicLink()) fail(`build tree contains a symlink: ${relativePath}`);
      if (info.isDirectory()) {
        await visit(absolutePath, relativePath);
      } else if (info.isFile()) {
        files.push({
          absolutePath,
          path: toPosix(relativePath),
          size: info.size,
        });
      } else {
        fail(`build tree contains a non-file, non-directory entry: ${relativePath}`);
      }
    }
  }

  await visit(rootPath, "");
  files.sort((left, right) => compareStrings(left.path, right.path));
  return files;
}

async function sha256File(filePath) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest("hex");
}

async function brotliContentInfo(filePath) {
  const hash = createHash("sha256");
  let decodedBytes = 0;
  const meter = new Transform({
    transform(chunk, _encoding, callback) {
      decodedBytes += chunk.length;
      hash.update(chunk);
      callback(null, chunk);
    },
  });

  try {
    await pipeline(
      createReadStream(filePath),
      createBrotliDecompress(),
      meter,
      new Writable({ write(_chunk, _encoding, callback) { callback(); } }),
    );
  } catch (error) {
    fail(`invalid Brotli source ${path.basename(filePath)}: ${error.message}`);
  }

  return { decodedBytes, decodedSha256: hash.digest("hex") };
}

async function loadManifest() {
  const manifestInfo = await lstat(DATA_FILES_PATH);
  if (manifestInfo.isSymbolicLink() || !manifestInfo.isFile()) {
    fail("repository data-files.json is not a regular file");
  }

  let manifest;
  try {
    manifest = JSON.parse(await readFile(DATA_FILES_PATH, "utf8"));
  } catch (error) {
    fail(`cannot parse repository data-files.json: ${error.message}`);
  }

  if (!Array.isArray(manifest) || manifest.length === 0) {
    fail("repository data-files.json must be a non-empty JSON array");
  }
  const names = new Set();
  for (const name of manifest) {
    if (
      typeof name !== "string" ||
      !/^[A-Za-z0-9][A-Za-z0-9._-]*\.json$/.test(name) ||
      name === ".json" ||
      names.has(name)
    ) {
      fail(`unsafe, duplicate, or invalid entry in data-files.json: ${String(name)}`);
    }
    names.add(name);
  }

  return [...names].sort(compareStrings);
}

function validatePublicUrl(name, value) {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) {
    fail(`${name} must be an explicit, whitespace-free public URL`);
  }
  if (/[\u0000-\u0020\\]/.test(value)) fail(`${name} contains whitespace or a backslash`);

  let parsed;
  try {
    parsed = new URL(value);
  } catch {
    fail(`${name} is not an absolute URL`);
  }
  if (parsed.protocol !== "https:") fail(`${name} must use https`);
  const credentialBearingComponents = [
    parsed.hostname,
    parsed.pathname,
    parsed.search,
    parsed.hash,
    parsed.username,
    parsed.password,
  ];
  for (const component of credentialBearingComponents) {
    let decodedComponent;
    try {
      decodedComponent = decodeURIComponent(component);
    } catch {
      fail(`${name} contains malformed URL encoding`);
    }
    if (SECRET_LIKE_PATTERN.test(component) || SECRET_LIKE_PATTERN.test(decodedComponent)) {
      fail(`${name} appears to contain a credential`);
    }
  }
  if (value.includes("?") || value.includes("#")) {
    fail(`${name} must not contain a query string or fragment`);
  }
  if (parsed.username || parsed.password) fail(`${name} must not include URL credentials`);
  if (!parsed.hostname) fail(`${name} must include a public hostname`);
  if (/%2f|%5c/i.test(parsed.pathname)) fail(`${name} contains an encoded path separator`);

  const authorityStart = value.indexOf("//") + 2;
  const pathStart = value.indexOf("/", authorityStart);
  const rawPath = pathStart === -1 ? "" : value.slice(pathStart);
  for (const segment of rawPath.split("/")) {
    let decodedSegment;
    try {
      decodedSegment = decodeURIComponent(segment);
    } catch {
      fail(`${name} contains malformed URL encoding`);
    }
    if (decodedSegment === "." || decodedSegment === "..") {
      fail(`${name} contains a dot-segment path component`);
    }
  }

  return parsed;
}

function validateConfiguration({ dataBaseUrl, cardDataUrl, engineWasmUrl }) {
  validatePublicUrl("DATA_BASE_URL", dataBaseUrl);
  const cardData = validatePublicUrl("CARD_DATA_URL", cardDataUrl);
  const engineWasm = validatePublicUrl("ENGINE_WASM_URL", engineWasmUrl);

  let cardFilename;
  try {
    cardFilename = decodeURIComponent(cardData.pathname.split("/").at(-1) ?? "");
  } catch {
    fail("CARD_DATA_URL has malformed path encoding");
  }
  if (!CARD_CORPUS_PATTERN.test(cardFilename)) {
    fail("CARD_DATA_URL must target card-data-<16 lowercase hex>.json");
  }

  let engineFilename;
  try {
    engineFilename = decodeURIComponent(engineWasm.pathname.split("/").at(-1) ?? "");
  } catch {
    fail("ENGINE_WASM_URL has malformed path encoding");
  }
  if (!ENGINE_WASM_PATTERN.test(engineFilename)) {
    fail("ENGINE_WASM_URL must target engine_wasm_bg-<16 lowercase hex>.wasm");
  }

  return { cardFilename, dataBaseUrl, cardDataUrl, engineWasmUrl, engineFilename };
}

function validateArtifactProvenance(value) {
  if (value === undefined || value === null) return null;
  if (
    typeof value !== "object" ||
    Array.isArray(value) ||
    (Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null)
  ) {
    fail("artifactProvenance must be a plain object containing only allowlisted fields");
  }

  const keys = Reflect.ownKeys(value);
  if (keys.length === 0 || keys.some((key) => typeof key !== "string" || !ARTIFACT_PROVENANCE_FIELDS.has(key))) {
    fail("artifactProvenance must contain at least one allowlisted field and no additional fields");
  }

  const normalized = {};
  for (const key of [
    "sourceRevision",
    "sourceArtifactSha256",
    "externalEngineWasmSha256",
    "buildRunId",
  ]) {
    if (!Object.hasOwn(value, key)) continue;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor || !("value" in descriptor)) {
      fail(`artifactProvenance.${key} must be a plain data property`);
    }
    const fieldValue = descriptor.value;
    if (key === "sourceRevision") {
      if (typeof fieldValue !== "string" || !/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(fieldValue)) {
        fail("artifactProvenance.sourceRevision must be a 40- or 64-character lowercase Git object ID");
      }
    } else if (key === "sourceArtifactSha256" || key === "externalEngineWasmSha256") {
      if (typeof fieldValue !== "string" || !/^[0-9a-f]{64}$/.test(fieldValue)) {
        fail(`artifactProvenance.${key} must be a 64-character lowercase SHA-256 digest`);
      }
    } else if (
      typeof fieldValue !== "string" ||
      !/^[A-Za-z0-9][A-Za-z0-9.:-]{0,63}$/.test(fieldValue) ||
      SECRET_LIKE_PATTERN.test(fieldValue)
    ) {
      fail("artifactProvenance.buildRunId must be a safe 1-64 character identifier");
    }
    normalized[key] = fieldValue;
  }

  if (Buffer.byteLength(JSON.stringify(normalized), "utf8") > MAX_ARTIFACT_PROVENANCE_JSON_BYTES) {
    fail(`artifactProvenance exceeds ${MAX_ARTIFACT_PROVENANCE_JSON_BYTES} UTF-8 bytes`);
  }
  return normalized;
}

function appendViteFilename(baseUrl, filename) {
  // Match client/vite.config.ts: `${base}/${filename}`. In particular, do not
  // resolve against a URL base, which would discard the final prefix segment.
  return `${baseUrl}/${filename}`;
}

function normalizeJavaScriptString(value) {
  return value
    .replaceAll("\\/", "/")
    .replace(/\\u002f/gi, "/")
    .replace(/\\x2f/gi, "/");
}

function extractStringLiterals(source) {
  const literals = [];
  const pattern = /"((?:\\.|[^"\\])*)"|'((?:\\.|[^'\\])*)'/g;
  for (const match of source.matchAll(pattern)) {
    literals.push(normalizeJavaScriptString(match[1] ?? match[2] ?? ""));
  }
  return literals;
}

function pathSuffixCandidate(value, filename) {
  const withoutQueryOrFragment = value.split(/[?#]/, 1)[0];
  return (
    withoutQueryOrFragment.endsWith(`/${filename}`) ||
    withoutQueryOrFragment === filename
  );
}

function isUrlOrPath(value) {
  return (
    /^https?:\/\//i.test(value) ||
    value.startsWith("/") ||
    value.startsWith("./") ||
    value.startsWith("../")
  );
}

function localReferenceCandidates(literal, bundlePath) {
  const value = normalizeJavaScriptString(literal);
  if (
    !value ||
    value.startsWith("//") ||
    /^[a-z][a-z\d+.-]*:/i.test(value)
  ) {
    return [];
  }

  const rawPath = value.split(/[?#]/, 1)[0];
  let decodedPath;
  try {
    decodedPath = decodeURIComponent(rawPath);
  } catch {
    return [];
  }
  if (!decodedPath || decodedPath.includes("\\")) return [];

  const candidates = new Set();
  const addCandidate = (candidate) => {
    const normalized = path.posix.normalize(candidate);
    if (
      normalized !== "." &&
      normalized !== ".." &&
      !normalized.startsWith("../") &&
      !normalized.startsWith("/")
    ) {
      candidates.add(normalized);
    }
  };

  const rootResolved = path.posix.normalize(path.posix.join("/", decodedPath)).slice(1);
  addCandidate(rootResolved);
  if (!decodedPath.startsWith("/")) {
    const bundleResolved = path.posix
      .normalize(path.posix.join("/", path.posix.dirname(bundlePath), decodedPath))
      .slice(1);
    addCandidate(bundleResolved);
  }
  return [...candidates];
}

async function assertNoLocalReferencesToRemovedArtifacts(inputFiles, removedPaths) {
  if (removedPaths.length === 0) return;
  const removed = new Set(removedPaths);
  const bundles = inputFiles.filter((entry) => /\.(?:js|mjs|cjs)$/.test(entry.path));
  for (const bundle of bundles) {
    const literals = extractStringLiterals(await readFile(bundle.absolutePath, "utf8"));
    for (const literal of literals) {
      for (const candidate of localReferenceCandidates(literal, bundle.path)) {
        if (removed.has(candidate)) {
          fail(`local JavaScript reference to offloaded artifact ${candidate} in ${bundle.path}; refusing to strip it`);
        }
      }
    }
  }
}

async function inspectBundleEvidence(inputFiles, manifestNames, config) {
  const bundles = inputFiles.filter((entry) => /\.(?:js|mjs|cjs)$/.test(entry.path));
  const literals = [];
  for (const bundle of bundles) {
    literals.push(...extractStringLiterals(await readFile(bundle.absolutePath, "utf8")));
  }

  const dataExpected = manifestNames.map((filename) =>
    appendViteFilename(config.dataBaseUrl, filename),
  );
  const dataMatches = dataExpected.filter((expected) => literals.includes(expected));
  const dataConflicts = literals.filter(
    (literal) =>
      isUrlOrPath(literal) &&
      manifestNames.some((filename) => pathSuffixCandidate(literal, filename)) &&
      !dataExpected.includes(literal),
  );
  if (dataConflicts.length > 0) {
    fail(`observable DATA_BASE_URL bundle mismatch: ${[...new Set(dataConflicts)].sort(compareStrings).join(", ")}`);
  }

  const cardCandidates = [...new Set(literals.filter((literal) => {
    if (!isUrlOrPath(literal)) return false;
    const filename = literal.split(/[?#]/, 1)[0].split("/").at(-1);
    return filename === "card-data.json" || CARD_CORPUS_PATTERN.test(filename ?? "");
  }))].sort(compareStrings);
  if (cardCandidates.some((candidate) => candidate !== config.cardDataUrl)) {
    fail(`observable CARD_DATA_URL bundle mismatch: ${cardCandidates.join(", ")}`);
  }

  const engineCandidates = [...new Set(literals.filter((literal) => {
    if (!isUrlOrPath(literal)) return false;
    const filename = literal.split(/[?#]/, 1)[0].split("/").at(-1);
    return filename === "engine_wasm_bg.wasm" || ENGINE_WASM_PATTERN.test(filename ?? "");
  }))].sort(compareStrings);
  if (engineCandidates.some((candidate) => candidate !== config.engineWasmUrl)) {
    fail(`observable ENGINE_WASM_URL bundle mismatch: ${engineCandidates.join(", ")}`);
  }

  function evidence(expected, observed) {
    const matches = observed.includes(expected);
    return {
      status: matches ? "MATCH" : "UNPROVEN",
      expected,
      observed: [...new Set(observed)].sort(compareStrings),
    };
  }

  return {
    DATA_BASE_URL: {
      status:
        dataMatches.length === dataExpected.length
          ? "MATCH"
          : dataMatches.length > 0
            ? "PARTIAL"
            : "UNPROVEN",
      expected: config.dataBaseUrl,
      expectedManifestUrlCount: dataExpected.length,
      matchedManifestUrlCount: dataMatches.length,
      matchedManifestUrls: [...dataMatches].sort(compareStrings),
    },
    CARD_DATA_URL: evidence(config.cardDataUrl, cardCandidates),
    ENGINE_WASM_URL: evidence(config.engineWasmUrl, engineCandidates),
  };
}

function urlEvidenceProven(evidence) {
  return Object.values(evidence).every((item) => item.status === "MATCH");
}

function computeMetrics(files) {
  const count = files.length;
  const totalBytes = files.reduce((total, file) => total + file.size, 0);
  const largest = [...files].sort((left, right) =>
    right.size - left.size || compareStrings(left.path, right.path),
  )[0] ?? null;
  return {
    fileCount: count,
    totalBytes,
    largestFile: largest ? { path: largest.path, bytes: largest.size } : null,
  };
}

export function validatePagesLimits(files) {
  const metrics = computeMetrics(files);
  const oversized = files
    .filter((file) => file.size > PAGES_MAX_FILE_BYTES)
    .sort((left, right) => compareStrings(left.path, right.path))[0];
  if (oversized) {
    fail(
      `Pages file limit exceeded: ${oversized.path} is ${oversized.size} bytes; limit is ${PAGES_MAX_FILE_BYTES}`,
    );
  }
  if (metrics.fileCount > PAGES_FREE_MAX_FILES) {
    fail(
      `Pages Free file-count limit exceeded: ${metrics.fileCount} files; limit is ${PAGES_FREE_MAX_FILES}`,
    );
  }
  return metrics;
}

function indexByPath(files) {
  return new Map(files.map((file) => [file.path, file]));
}

async function artifactInfo(entry) {
  const statInfo = await stat(entry.absolutePath);
  const encodedSha256 = await sha256File(entry.absolutePath);
  if (entry.path.endsWith(".br")) {
    const decoded = await brotliContentInfo(entry.absolutePath);
    return {
      path: entry.path,
      representation: "brotli",
      encodedBytes: statInfo.size,
      encodedSha256,
      decodedBytes: decoded.decodedBytes,
      decodedContentSha256: decoded.decodedSha256,
    };
  }
  return {
    path: entry.path,
    representation: "identity",
    encodedBytes: statInfo.size,
    encodedSha256,
    decodedBytes: statInfo.size,
    decodedContentSha256: encodedSha256,
  };
}

function sourcePair(index, filename) {
  return {
    identity: index.get(filename) ?? null,
    brotli: index.get(`${filename}.br`) ?? null,
  };
}

async function validateLogicalSource(index, filename) {
  const validated = await validateOptionalLogicalSource(index, filename);
  if (!validated) {
    fail(`required build input is missing: ${filename} (or ${filename}.br)`);
  }
  return validated;
}

function unverifiedAbsentLogicalObject(key, publicUrl) {
  return {
    key,
    publicUrl,
    expectedContentEncoding: "br",
    sourceRepresentation: null,
    sourcePath: null,
    encodedBytes: null,
    encodedBytesSha256: null,
    encodedBytesStatus: "UNVERIFIED_ABSENT",
    decodedBytes: null,
    decodedContentSha256: null,
    sourceArtifacts: [],
  };
}

async function inspectOffloadedSources(index, manifestNames, config) {
  const logicalObjects = [];
  const removed = new Set();
  const sourceArtifacts = [];
  const unverifiedAbsent = [];

  for (const filename of manifestNames) {
    const validated = await validateOptionalLogicalSource(index, filename);
    if (!validated) {
      logicalObjects.push(
        unverifiedAbsentLogicalObject(filename, appendViteFilename(config.dataBaseUrl, filename)),
      );
      unverifiedAbsent.push(filename);
      continue;
    }
    const { sourceArtifacts: sources, selected } = validated;
    sourceArtifacts.push(...sources);
    if (index.has(filename)) removed.add(filename);
    if (index.has(`${filename}.br`)) removed.add(`${filename}.br`);
    logicalObjects.push({
      key: filename,
      publicUrl: appendViteFilename(config.dataBaseUrl, filename),
      expectedContentEncoding: "br",
      sourceRepresentation: selected.representation,
      sourcePath: selected.path,
      encodedBytes: selected.representation === "brotli" ? selected.encodedBytes : null,
      encodedBytesSha256: selected.representation === "brotli" ? selected.encodedSha256 : null,
      encodedBytesStatus: selected.representation === "brotli" ? "VERIFIED_LOCAL" : "NOT_PRESENT_UPLOAD_TRANSFORM_REQUIRED",
      decodedBytes: selected.decodedBytes,
      decodedContentSha256: selected.decodedContentSha256,
      sourceArtifacts: sources.map(({ path: sourcePath, representation, encodedBytes, encodedSha256, decodedBytes, decodedContentSha256 }) => ({
        path: sourcePath,
        representation,
        encodedBytes,
        encodedSha256,
        decodedBytes,
        decodedContentSha256,
      })),
    });
  }

  const plainCard = await validateOptionalLogicalSource(index, "card-data.json");
  if (plainCard) {
    sourceArtifacts.push(...plainCard.sourceArtifacts);
    if (index.has("card-data.json")) removed.add("card-data.json");
    if (index.has("card-data.json.br")) removed.add("card-data.json.br");
  }

  const rootHashedFiles = [...new Set(
    [...index.values()]
      .filter((entry) => !entry.path.includes("/"))
      .map((entry) => entry.path.endsWith(".br") ? entry.path.slice(0, -3) : entry.path)
      .filter((filename) => CARD_CORPUS_PATTERN.test(filename))
      .map((filename) => filename.replace(/\.json$/, "")),
  )].sort(compareStrings);
  const hashedNames = new Set(rootHashedFiles);
  for (const sourceName of rootHashedFiles) {
    const filename = `${sourceName}.json`;
    const { sourceArtifacts: sources, selected } = await validateLogicalSource(index, filename);
    sourceArtifacts.push(...sources);
    const [, hashPrefix] = CARD_CORPUS_PATTERN.exec(filename);
    if (!selected.decodedContentSha256.startsWith(hashPrefix)) {
      fail(`content-addressed card corpus hash mismatch: ${filename}`);
    }
    if (index.has(filename)) removed.add(filename);
    if (index.has(`${filename}.br`)) removed.add(`${filename}.br`);

    const isConfiguredCorpus = filename === config.cardFilename;
    logicalObjects.push({
      key: filename,
      publicUrl: isConfiguredCorpus
        ? config.cardDataUrl
        : appendViteFilename(config.dataBaseUrl, filename),
      expectedContentEncoding: "br",
      sourceRepresentation: selected.representation,
      sourcePath: selected.path,
      encodedBytes: selected.representation === "brotli" ? selected.encodedBytes : null,
      encodedBytesSha256: selected.representation === "brotli" ? selected.encodedSha256 : null,
      encodedBytesStatus: selected.representation === "brotli" ? "VERIFIED_LOCAL" : "NOT_PRESENT_UPLOAD_TRANSFORM_REQUIRED",
      decodedBytes: selected.decodedBytes,
      decodedContentSha256: selected.decodedContentSha256,
      sourceArtifacts: sources.map(({ path: sourcePath, representation, encodedBytes, encodedSha256, decodedBytes, decodedContentSha256 }) => ({
        path: sourcePath,
        representation,
        encodedBytes,
        encodedSha256,
        decodedBytes,
        decodedContentSha256,
      })),
    });
  }

  const configuredCorpusName = config.cardFilename.replace(/\.json$/, "");
  if (!hashedNames.has(configuredCorpusName)) {
    if (!manifestNames.includes(config.cardFilename)) {
      logicalObjects.push(unverifiedAbsentLogicalObject(config.cardFilename, config.cardDataUrl));
    }
    unverifiedAbsent.push(config.cardFilename);
  }

  if (plainCard && hashedNames.has(config.cardFilename.replace(/\.json$/, ""))) {
    const selectedHash = await validateLogicalSource(index, config.cardFilename);
    if (
      plainCard.selected.decodedBytes !== selectedHash.selected.decodedBytes ||
      plainCard.selected.decodedContentSha256 !== selectedHash.selected.decodedContentSha256
    ) {
      fail("card-data.json does not match the configured content-addressed card corpus");
    }
  }

  return {
    logicalObjects: logicalObjects.sort((left, right) => compareStrings(left.key, right.key)),
    removed: [...removed].sort(compareStrings),
    sourceArtifacts: sourceArtifacts.sort((left, right) => compareStrings(left.path, right.path)),
    unverifiedAbsent: [...new Set(unverifiedAbsent)].sort(compareStrings),
  };
}

async function validateOptionalLogicalSource(index, filename) {
  const pair = sourcePair(index, filename);
  if (!pair.identity && !pair.brotli) return null;
  const identityInfo = pair.identity ? await artifactInfo(pair.identity) : null;
  const brotliInfo = pair.brotli ? await artifactInfo(pair.brotli) : null;
  if (
    identityInfo &&
    brotliInfo &&
    (identityInfo.decodedBytes !== brotliInfo.decodedBytes ||
      identityInfo.decodedContentSha256 !== brotliInfo.decodedContentSha256)
  ) {
    fail(`Brotli companion does not decode to ${filename}`);
  }
  return {
    sourceArtifacts: [identityInfo, brotliInfo].filter(Boolean),
    selected: brotliInfo ?? identityInfo,
  };
}

async function inspectWasm(inputFiles, inputIndex, config) {
  const jsFiles = inputFiles.filter((entry) => /\.(?:js|mjs|cjs)$/.test(entry.path));
  const refs = [];
  const wasmReferencePattern = /"((?:\\.|[^"\\])*)"|'((?:\\.|[^'\\])*)'/g;

  for (const bundle of jsFiles) {
    const source = await readFile(bundle.absolutePath, "utf8");
    for (const match of source.matchAll(wasmReferencePattern)) {
      const rawValue = normalizeJavaScriptString(match[1] ?? match[2] ?? "");
      if (!/\.wasm(?:[?#].*)?$/i.test(rawValue)) continue;
      if (/^(?:https?:)?\/\//i.test(rawValue) || rawValue.startsWith("data:")) continue;

      const pathPart = rawValue.split(/[?#]/, 1)[0];
      let decodedPath;
      try {
        decodedPath = decodeURIComponent(pathPart);
      } catch {
        fail(`invalid encoded WASM reference in ${bundle.path}: ${rawValue}`);
      }
      if (decodedPath.includes("\\")) fail(`unsafe WASM reference in ${bundle.path}: ${rawValue}`);
      const target = decodedPath.startsWith("/")
        ? path.posix.normalize(decodedPath.slice(1))
        : path.posix.normalize(path.posix.join(path.posix.dirname(bundle.path), decodedPath));
      if (target === ".." || target.startsWith("../") || target.startsWith("/")) {
        fail(`WASM reference escapes the build directory: ${bundle.path} -> ${rawValue}`);
      }

      const file = inputIndex.get(target);
      if (!file) fail(`referenced local WASM is missing: ${bundle.path} -> ${target}`);
      refs.push({ from: bundle.path, reference: rawValue, target });
    }
  }

  const localWasm = [];
  for (const entry of inputFiles.filter((file) => file.path.endsWith(".wasm"))) {
    localWasm.push({
      path: entry.path,
      bytes: entry.size,
      sha256: await sha256File(entry.absolutePath),
    });
  }
  localWasm.sort((left, right) => compareStrings(left.path, right.path));

  const configuredEngineMatches = inputFiles.filter(
    (entry) => path.posix.basename(entry.path) === config.engineFilename,
  );
  if (configuredEngineMatches.length > 1) {
    fail(`multiple local sources match configured engine WASM: ${config.engineFilename}`);
  }
  const configuredEngine = configuredEngineMatches[0] ?? null;
  let configuredEngineBytes;
  if (configuredEngine) {
    const sha256 = await sha256File(configuredEngine.absolutePath);
    const expectedPrefix = ENGINE_WASM_PATTERN.exec(config.engineFilename)[1];
    if (!sha256.startsWith(expectedPrefix)) {
      fail(`content-addressed engine WASM hash mismatch: ${config.engineFilename}`);
    }
    configuredEngineBytes = {
      path: configuredEngine.path,
      bytes: configuredEngine.size,
      sha256,
      status: "VERIFIED_LOCAL",
    };
  } else {
    configuredEngineBytes = {
      path: config.engineFilename,
      bytes: null,
      sha256: null,
      status: "UNVERIFIED_ABSENT",
    };
  }

  return {
    references: refs.sort((left, right) =>
      compareStrings(left.from, right.from) || compareStrings(left.target, right.target),
    ),
    localFiles: localWasm,
    configuredEngineBytes,
  };
}

async function readHeadersSource() {
  const info = await lstat(HEADERS_PATH);
  if (info.isSymbolicLink() || !info.isFile()) {
    fail("pinned upstream Cloudflare _headers source is not a regular file");
  }
  return {
    bytes: info.size,
    sha256: await sha256File(HEADERS_PATH),
  };
}

async function copyPreparedTree(inputFiles, outputPath, omittedPaths, headersInfo) {
  await mkdir(outputPath);
  try {
    for (const entry of inputFiles) {
      if (omittedPaths.has(entry.path) || entry.path === "_headers") continue;
      const target = path.join(outputPath, ...entry.path.split("/"));
      await mkdir(path.dirname(target), { recursive: true });
      await copyFile(entry.absolutePath, target);
    }
    await copyFile(HEADERS_PATH, path.join(outputPath, "_headers"));

    const finalFiles = await collectTree(outputPath);
    const actualHeaders = finalFiles.find((entry) => entry.path === "_headers");
    if (!actualHeaders || actualHeaders.size !== headersInfo.bytes) {
      fail("copied _headers does not match the pinned upstream source");
    }
    if ((await sha256File(actualHeaders.absolutePath)) !== headersInfo.sha256) {
      fail("copied _headers hash does not match the pinned upstream source");
    }
    const pages = validatePagesLimits(finalFiles);
    return { finalFiles, pages };
  } catch (error) {
    await rm(outputPath, { recursive: true, force: true });
    throw error;
  }
}

export async function preparePagesArtifacts({
  inputDir,
  outputDir,
  dataBaseUrl,
  cardDataUrl,
  engineWasmUrl,
  attestBuildUrls = false,
  artifactProvenance = null,
}) {
  if (typeof attestBuildUrls !== "boolean") fail("attestBuildUrls must be a boolean");
  const callerSuppliedProvenance = validateArtifactProvenance(artifactProvenance);

  const inputPath = await requireDirectoryWithoutSymlinks(inputDir, "input");
  const outputPath = path.resolve(outputDir);
  await assertOutputAbsentAndSafe(outputPath);
  if (isSameOrNested(inputPath, outputPath) || isSameOrNested(outputPath, inputPath)) {
    fail("input and output directories must be separate and must not be nested");
  }

  const config = validateConfiguration({ dataBaseUrl, cardDataUrl, engineWasmUrl });
  const manifestNames = await loadManifest();
  const inputFiles = await collectTree(inputPath);
  const inputIndex = indexByPath(inputFiles);
  const indexHtml = inputIndex.get("index.html");
  if (!indexHtml) fail("required build input is missing: index.html");

  const source404 = inputIndex.get("404.html");
  let notFoundHandling = "ABSENT";
  const omittedPaths = new Set();
  if (source404) {
    const [indexBytes, notFoundBytes] = await Promise.all([
      readFile(indexHtml.absolutePath),
      readFile(source404.absolutePath),
    ]);
    if (!indexBytes.equals(notFoundBytes)) {
      fail("custom 404.html is not byte-identical to index.html; an explicit routing decision is required");
    }
    omittedPaths.add("404.html");
    notFoundHandling = "OMITTED_IDENTICAL_TO_INDEX";
  }

  const headersInfo = await readHeadersSource();
  const suppliedHeaders = inputIndex.get("_headers");
  if (suppliedHeaders) {
    if (
      suppliedHeaders.size !== headersInfo.bytes ||
      (await sha256File(suppliedHeaders.absolutePath)) !== headersInfo.sha256
    ) {
      fail("input _headers conflicts with the pinned upstream Cloudflare _headers");
    }
  }

  const offloaded = await inspectOffloadedSources(inputIndex, manifestNames, config);
  await assertNoLocalReferencesToRemovedArtifacts(inputFiles, offloaded.removed);
  const bundleEvidence = await inspectBundleEvidence(inputFiles, manifestNames, config);
  for (const omitted of offloaded.removed) omittedPaths.add(omitted);
  const wasm = await inspectWasm(inputFiles, inputIndex, config);

  const created = await copyPreparedTree(inputFiles, outputPath, omittedPaths, headersInfo);
  const localEngineVerified = wasm.configuredEngineBytes.status === "VERIFIED_LOCAL";
  const blockers = [];
  if (!attestBuildUrls) blockers.push("caller did not attest that the input build used the supplied URL values");
  if (!urlEvidenceProven(bundleEvidence)) blockers.push("one or more supplied build URL values are not fully observable in the input JavaScript bundles");
  blockers.push("an operator must verify public URL ownership, object-key routing, and the serving Content-Encoding");
  if (!localEngineVerified) blockers.push("configured external engine WASM bytes are absent, so their byte hash and URL correspondence are unverified");
  if (offloaded.unverifiedAbsent.length > 0) {
    blockers.push(
      `externally configured JSON objects are absent locally, so their byte and content hashes are unverified: ${offloaded.unverifiedAbsent.join(", ")}`,
    );
  }

  const removedArtifacts = offloaded.sourceArtifacts.map((source) => ({ ...source }));
  removedArtifacts.sort((left, right) => compareStrings(left.path, right.path));

  return {
    schemaVersion: 1,
    tool: "scripts/prepare-cloudflare-pages.mjs",
    artifactPreparation: "PASS",
    limits: {
      maxFileBytes: PAGES_MAX_FILE_BYTES,
      maxFreeFiles: PAGES_FREE_MAX_FILES,
    },
    configuration: {
      publicAssetUrls: {
        DATA_BASE_URL: config.dataBaseUrl,
        CARD_DATA_URL: config.cardDataUrl,
        ENGINE_WASM_URL: config.engineWasmUrl,
      },
      callerAttestation: {
        buildUsedTheseUrlValues: attestBuildUrls,
      },
    },
    callerSuppliedArtifactProvenance: callerSuppliedProvenance
      ? {
          status: "CALLER_SUPPLIED_NOT_LOCALLY_VERIFIED",
          ...callerSuppliedProvenance,
        }
      : { status: "NOT_SUPPLIED" },
    bundleEvidence,
    offloadedSources: removedArtifacts,
    publicObjects: offloaded.logicalObjects,
    removedFromPages: [...omittedPaths].sort(compareStrings),
    wasm: {
      localFiles: wasm.localFiles,
      localReferences: wasm.references,
      configuredEngineObject: {
        url: config.engineWasmUrl,
        bytes: wasm.configuredEngineBytes.bytes,
        sha256: wasm.configuredEngineBytes.sha256,
        byteVerification: wasm.configuredEngineBytes.status,
        urlFilenameHashPrefix: ENGINE_WASM_PATTERN.exec(config.engineFilename)[1],
      },
    },
    spa: {
      indexHtml: "PRESERVED",
      topLevel404: notFoundHandling,
      pagesDefaultSpaFallback: "EXPECTED_WHEN_TOP_LEVEL_404_IS_ABSENT",
    },
    pagesOutput: created.pages,
    deploymentReadiness: {
      status: "BLOCKED",
      blockers,
    },
  };
}

function parseArguments(args) {
  const values = new Map();
  let attestBuildUrls = false;
  const valueFlags = new Set([
    "--input",
    "--output",
    "--data-base-url",
    "--card-data-url",
    "--engine-wasm-url",
    "--artifact-provenance-json",
  ]);
  const requiredValueFlags = new Set([
    "--input",
    "--output",
    "--data-base-url",
    "--card-data-url",
    "--engine-wasm-url",
  ]);
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    if (flag === "--attest-build-urls") {
      if (attestBuildUrls) fail("duplicate argument: --attest-build-urls");
      attestBuildUrls = true;
      continue;
    }
    if (!valueFlags.has(flag)) fail(`unknown argument: ${flag}`);
    if (values.has(flag)) fail(`duplicate argument: ${flag}`);
    const value = args[index + 1];
    if (!value || value.startsWith("--")) fail(`missing value for ${flag}`);
    values.set(flag, value);
    index += 1;
  }

  for (const flag of requiredValueFlags) {
    if (!values.has(flag)) fail(`required argument is missing: ${flag}`);
  }

  let artifactProvenance = null;
  if (values.has("--artifact-provenance-json")) {
    const json = values.get("--artifact-provenance-json");
    if (Buffer.byteLength(json, "utf8") > MAX_ARTIFACT_PROVENANCE_JSON_BYTES) {
      fail(`--artifact-provenance-json exceeds ${MAX_ARTIFACT_PROVENANCE_JSON_BYTES} UTF-8 bytes`);
    }
    try {
      artifactProvenance = JSON.parse(json);
    } catch {
      fail("--artifact-provenance-json must be valid JSON");
    }
  }
  return {
    inputDir: values.get("--input"),
    outputDir: values.get("--output"),
    dataBaseUrl: values.get("--data-base-url"),
    cardDataUrl: values.get("--card-data-url"),
    engineWasmUrl: values.get("--engine-wasm-url"),
    attestBuildUrls,
    artifactProvenance,
  };
}

async function main() {
  const report = await preparePagesArtifacts(parseArguments(process.argv.slice(2)));
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    process.stderr.write(`prepare-cloudflare-pages: ${error.message}\n`);
    process.exitCode = 1;
  });
}
