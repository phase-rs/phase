import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { isOpenableExternalUrl } from "../../services/openExternal";
import { parseWebSocketUrl } from "../multiplayerServer";

// The Helm chart refuses an address the client would silently discard. That only
// holds while the chart's grammar for a value admits no more than the client's
// validator for that value, and the two live in different languages, so the
// grammar is read out of the template here rather than copied: a change to one
// without the other fails this test instead of drifting quietly.

// Walk up from the working directory so this resolves whether the suite runs
// from client/ or from the repo root; throws rather than silently skipping.
const HELPERS = (() => {
  const rel = "deploy/helm/phase-server/templates/_helpers.tpl";
  for (let dir = process.cwd(); ; dir = dirname(dir)) {
    const candidate = resolve(dir, rel);
    if (existsSync(candidate)) return candidate;
    if (dirname(dir) === dir) throw new Error(`could not locate ${rel} from ${process.cwd()}`);
  }
})();

function capture(src: string, pattern: RegExp, what: string): string {
  const m = src.match(pattern);
  if (!m) throw new Error(`no ${what} found in ${HELPERS}`);
  return m[1];
}

// Helm matches the chart's patterns with Go's RE2 and this file with JavaScript's
// RegExp, and the two read some constructs differently: `\s` is ASCII-only in RE2
// and Unicode in JavaScript, so `\S` admits a vertical tab in one and refuses it
// in the other. Each token below was compared under helm's regexMatch and
// JavaScript's RegExp at every code point, alone and inside the chart's patterns,
// and read the same. Compare a construct the same way before adding it.
const SHARED_TOKENS = new Set([
  "$", "(", ")", "*", "+", "-", "/", "0", "1", "2", "3", "5", "6", ":", "?",
  '[!-"$-~]', "[!-~]", "[/?#]", "[/?]", "[0-2]", "[0-4]", "[0-5]", "[0-9A-Fa-f]",
  "[0-9]", "[1-5]", "[1-9]", "[A-Za-z0-9_-]", "[A-Za-z]", "[Ff]", "[Nn]", "[Xx]", "[a-z]",
  "\\.", "\\[", "\\]", "^", "h", "p", "s", "t", "w",
  "{1,2}", "{1,3}", "{1,4}", "{1,5}", "{1,6}", "{1,7}", "{2}", "{3}", "{4}", "{7}", "|",
]);

const firstCodePoint = (s: string) => [...s.slice(0, 2)][0];

function lexBracket(re: string, start: number): string {
  let j = start + 1;
  if (re[j] === "^") j++;
  if (re[j] === "]") j++;
  while (j < re.length && re[j] !== "]") {
    const posixEnd = re.startsWith("[:", j) ? re.indexOf(":]", j) : -1;
    j = re[j] === "\\" ? j + 2 : posixEnd > 0 ? posixEnd + 2 : j + 1;
  }
  if (j >= re.length) throw new Error(`unclosed [ in the chart pattern ${JSON.stringify(re)}`);
  return re.slice(start, j + 1);
}

// Splits a pattern into whole constructs so each is admitted or refused whole: a
// bracket expression, an escape, a group opener with its `?` extension (`(?:`,
// `(?=`), a quantifier or count with its lazy `?`, or one character. Read piece
// by piece, `(?:` would pass as `(` and `?`, which are both in the set.
function lexPattern(re: string): string[] {
  const tokens: string[] = [];
  for (let i = 0; i < re.length; ) {
    const rest = re.slice(i);
    let token: string;
    if (rest[0] === "\\") {
      if (rest.length < 2) throw new Error(`trailing \\ in the chart pattern ${JSON.stringify(re)}`);
      token = rest.match(/^\\(?:[xpP]\{[^}]*\}|Q[\s\S]*?(?:\\E|$))/)?.[0] ?? `\\${firstCodePoint(rest.slice(1))}`;
    } else if (rest[0] === "[") {
      token = lexBracket(re, i);
    } else {
      token = rest.match(/^(?:\(\?[^:=!)>]*[:=!)>]?|[*+?]\??|\{\d+(?:,\d*)?\}\??)/)?.[0] ?? firstCodePoint(rest);
    }
    tokens.push(token);
    i += token.length;
  }
  return tokens;
}

// Every chart pattern reaches RegExp through this guard, as the string helm compiles.
function guardPattern(re: string): string {
  for (const token of lexPattern(re)) {
    if (!SHARED_TOKENS.has(token)) {
      throw new Error(
        `token ${JSON.stringify(token)} in the chart pattern ${JSON.stringify(re)} has not been compared under helm's regexMatch and JavaScript's RegExp; compare it under both before adding it to SHARED_TOKENS`,
      );
    }
  }
  return re;
}

const compilePattern = (re: string) => new RegExp(guardPattern(re));

// helm assembles a shape with printf and this file with replace, which agree only
// on a format holding one %, as %s (printf renders %% as a single %). The tokens
// are checked after assembly, which can join two members into a construct that is
// not one: "(" and "?:" make "(?:".
function compileShape(format: string, authority: string): RegExp {
  if (format.split("%").length !== 2 || !format.includes("%s")) {
    throw new Error(`the printf format ${JSON.stringify(format)} must hold exactly one %, as %s`);
  }
  return compilePattern(format.replace("%s", () => authority));
}

// Helm reads every file under the chart's templates/ directory, at any depth and
// with any extension, and each subchart's templates, as one set of named
// templates, and a define in one file can replace the same name's define in
// another, depending on the files' names and depths. So each name the extraction
// reads must be defined in exactly one of those files. No subchart is read here,
// so a charts/ directory throws.
function chartTemplates(): Map<string, string> {
  const charts = resolve(dirname(HELPERS), "../charts");
  if (existsSync(charts)) throw new Error(`${charts} exists, and this test reads no subchart's templates`);
  const entries = readdirSync(dirname(HELPERS), { recursive: true, withFileTypes: true });
  return new Map(
    entries
      .filter((entry) => entry.isFile())
      .map((entry): [string, string] => {
        const file = resolve(entry.parentPath, entry.name);
        return [file, readFileSync(file, "utf8")];
      }),
  );
}

function requireOneDefinition(templates: Map<string, string>, name: string): void {
  const sites = [...templates].flatMap(([file, text]) => text.split(`define "${name}"`).slice(1).map(() => file));
  if (sites.length !== 1) {
    throw new Error(`template "${name}" is defined ${sites.length} times among the chart's templates, in ${sites.join(" and ")}`);
  }
}

// The chart's verdict on web.<key>: that validator's anchored shape around the
// shared authority grammar, minus hosts with a punycode label. Each capture spans
// the whole action helm evaluates, and each template name read here must be
// defined once among the chart's templates, so these edits stop the extraction
// instead of changing what helm compiles: a pipe on the include, a second action
// in the authority define, a rewrite between the shape and its match, and a
// second `define "<name>"` of a name read here in any template file.
function chartAccepts(key: string, templates = chartTemplates()): (value: string) => boolean {
  const src = templates.get(HELPERS) ?? "";
  const authority = capture(
    src,
    /define "phase-server\.urlAuthorityPattern" -\}\}\n\{\{- `([^`]+)` -\}\}\n\{\{- end -\}\}/,
    "urlAuthorityPattern raw string",
  );
  const punycodeSource = capture(
    src,
    /define "phase-server\.refusePunycodeHost" -\}\}\n\{\{- if regexMatch `([^`]+)` \.url -\}\}/,
    "refusePunycodeHost raw string",
  );
  const start = src.search(new RegExp(`\\$url := \\.Values\\.web\\.${key} -\\}\\}`));
  if (start < 0) throw new Error(`no web.${key} validator found in ${HELPERS}`);
  const validator = capture(src.slice(0, start), /define "([^"]+)" -\}\}\n\{\{- $/, `web.${key} validator define`);
  const end = src.indexOf('{{- define "', start);
  const body = src.slice(start, end < 0 ? undefined : end);
  // The verdict below subtracts the refusal, so a validator that stopped
  // performing it would otherwise still pass.
  if (!body.includes(`include "phase-server.refusePunycodeHost" (dict "key" "web.${key}"`)) {
    throw new Error(`the web.${key} validator does not include phase-server.refusePunycodeHost`);
  }
  const format = capture(
    body,
    /\{\{- \$re := printf `([^`]+)` \(include "phase-server\.urlAuthorityPattern" \.\) -\}\}\n\{\{- if not \(regexMatch \$re \$url\) -\}\}/,
    `web.${key} shape`,
  );
  for (const name of ["phase-server.urlAuthorityPattern", "phase-server.refusePunycodeHost", validator]) {
    requireOneDefinition(templates, name);
  }
  // Compiled only after every capture matched and every name read is defined
  // once, so a template the extraction cannot read fails as unreadable rather
  // than on one of its tokens.
  const punycode = compilePattern(punycodeSource);
  const shape = compileShape(format, authority);
  return (v) => shape.test(v) && !punycode.test(v);
}

// Generated, not hand-listed. The first version of this guard listed sample
// addresses and missed a whole class — bracketed hosts with two elisions, and
// ones with too few groups and no elision — because nobody thought to write
// them down. Enumerating the shape instead of the examples is what makes the
// subset claim mean something.
const bracketed = new Set<string>();
for (let n = 1; n <= 10; n++) bracketed.add(Array(n).fill("1").join(":"));
for (let a = 0; a <= 4; a++)
  for (let b = 0; b <= 4; b++)
    bracketed.add(`${Array(a).fill("1").join(":")}::${Array(b).fill("2").join(":")}`);
for (let a = 0; a <= 3; a++)
  for (let b = 0; b <= 3; b++)
    for (let c = 0; c <= 3; c++)
      bracketed.add(
        `${Array(a).fill("1").join(":")}::${Array(b).fill("2").join(":")}::${Array(c).fill("3").join(":")}`,
      );
for (const h of [
  "::1", "::", "::ffff:192.168.1.1", "1:2:3:4:5:6:7:8", "2001:db8::8a2e:370:7334",
  "gggg::1", "1:2:3:4:5:6:7:8:9", "12345::1", "1::2::3", "::ffff:999.1.1.1", "x",
]) bracketed.add(h);

// Dotted-numeric hosts are the second generated class. URL parsing decides a
// host is an IPv4 attempt from its final label, so these are not hostnames
// that happen to contain digits — they are addresses that fail to parse.
const numeric = new Set<string>();
const MAGS = ["0", "1", "99", "127", "192", "255", "256", "999", "1000", "65535",
              "4294967295", "4294967296", "01", "0x7f", "0xff", "00"];
for (const n of [1, 2, 3, 4, 5])
  for (const m of MAGS) numeric.add(Array(n).fill(m).join("."));
for (const h of ["192.168.1.5", "255.255.255.255", "0.0.0.0", "1.2.3", "127.1",
                 "2130706433", "1.2.3.4.5", "999.999.999.999", "256.1.1.1",
                 "0x7f.0.0.1", "example.com", "sub.example.com", "localhost",
                 "host-1.example.com"]) numeric.add(h);

// URL parsing throws on an xn-- label that is not valid punycode, in any label
// and either case; the last host is valid punycode, which the chart may refuse.
const punycode = ["xn--a.example", "XN--a.example", "a.xn--a", "xn--.example", "xn--bcher-kva.example"];

const HOSTS = [...numeric, ...[...bracketed].map((h) => `[${h}]`), ...punycode];

const ROWS = [
  {
    key: "defaultMultiplayerServerUrl",
    clientAccepts: (v: string) => parseWebSocketUrl(v) !== null,
    corpus: [
      ...HOSTS.map((h) => `wss://${h}/ws`),
      "wss://play.example.com/ws",
      "ws://192.168.1.5:9374/ws",
      "wss://play.example.com/ws?region=eu",
      "wss://play.example.com:65535/ws",
      "wss://play.example.com:0/ws",
      "wss://play.example.com:abc/ws",
      "wss://play.example.com:99999/ws",
      "wss://play.example.com:-1/ws",
      "wss://[::1/ws",
      "wss://[]/ws",
      "wss://]::1[/ws",
      "wss://:9374/ws",
      "wss://@/ws",
      "wss://%00.com/ws",
      "wss://play.example.com bad",
      "wss://play.example.com\tbad",
      "wss://play.example.com/ws#lobby",
      "wss://play.example.com/ws#",
      "https://play.example.com",
      "play.example.com",
      "wss://",
    ],
    ordinary: ["wss://play.example.com/ws", "ws://192.168.1.5:9374/ws", "wss://[::1]:9374/ws"],
  },
  {
    key: "previewSiteUrl",
    clientAccepts: isOpenableExternalUrl,
    corpus: [
      // The shape admits a fragment, so every generated host is tried with one.
      ...HOSTS.flatMap((h) => [`https://${h}/`, `http://${h}/p?q=1#f`]),
      "https://preview.example.com:65535/",
      "https://preview.example.com:0/",
      "https://preview.example.com:abc/",
      "https://preview.example.com:99999/",
      "https://preview.example.com:-1/",
      "https://[::1/",
      "https://[]/",
      "https://]::1[/",
      "https://:8443/",
      "https://@/",
      "https://%00.com/",
      "https://preview.example.com/ bad",
      "https://preview.example.com/\tbad",
      "https://preview.example.com/#",
      "javascript:alert(1)",
      "wss://preview.example.com",
      "phase-preview.example.com",
      "https://",
    ],
    ordinary: [
      "https://phase-preview.example.com",
      "http://192.168.1.5:8080/",
      "https://[::1]:8443/p",
      "https://preview.example.com/play?x=1#top",
    ],
  },
];

describe.each(ROWS)("chart web.$key grammar vs the client", ({ key, clientAccepts, corpus, ordinary }) => {
  it("never admits an address the client would discard", () => {
    const admitted = corpus.filter(chartAccepts(key));
    expect(admitted.length).toBeGreaterThan(0);
    expect(admitted.filter((v) => !clientAccepts(v))).toEqual([]);
  });

  // Without this the case above passes for a chart regex that accepts nothing.
  it("still admits the ordinary addresses operators configure", () => {
    const accepts = chartAccepts(key);
    for (const v of ordinary) {
      expect(accepts(v), v).toBe(true);
      expect(clientAccepts(v), v).toBe(true);
    }
  });
});

describe("chart pattern extraction", () => {
  it("compiles the template's own patterns", () => {
    for (const { key } of ROWS) expect(() => chartAccepts(key)).not.toThrow();
  });

  it.each([
    ["\\s", "^s\\s$"],
    ["{01}", "^s{01}$"],
    ["(?=", "^(?=s)s$"],
  ])("refuses the uncompared token %s", (token, pattern) => {
    expect(() => compilePattern(pattern)).toThrow(`token ${JSON.stringify(token)} in`);
  });

  it.each([
    ["[^0]", "^s[%s]$", "^0"],
    ["(?:", "^s(%s)$", "?:s"],
  ])("refuses %s when only the assembled shape holds it", (token, format, authority) => {
    expect(() => compileShape(format, authority)).toThrow(`token ${JSON.stringify(token)} in`);
  });

  it.each(["^s%%%s$", "^%s%s$"])("refuses the printf format %s", (format) => {
    expect(() => compileShape(format, "s")).toThrow("must hold exactly one %, as %s");
  });

  const templates = chartTemplates();
  const helpers = readFileSync(HELPERS, "utf8");
  const include = '(include "phase-server.urlAuthorityPattern" .) -}}';
  it.each([
    ["a piped authority include", include, '(include "phase-server.urlAuthorityPattern" . | replace "{1,4}" "{1,9}") -}}'],
    ["a shape rewritten before its match", `${include}\n`, `${include}\n{{- $re = replace "{1,4}" "{1,9}" $re -}}\n`],
    ["a second action in the authority define", "` -}}\n{{- end -}}", "` -}}\n{{- `(\\.x)?` -}}\n{{- end -}}"],
  ])("stops at %s", (_name, from, to) => {
    const edited = new Map(templates).set(HELPERS, helpers.split(from).join(to));
    for (const { key } of ROWS) expect(() => chartAccepts(key, edited)).toThrow(/^no .* found in /);
  });

  const other = resolve(dirname(HELPERS), "_aa.tpl");
  it.each([
    ["phase-server.urlAuthorityPattern", "defaultMultiplayerServerUrl"],
    ["phase-server.urlAuthorityPattern", "previewSiteUrl"],
    ["phase-server.refusePunycodeHost", "defaultMultiplayerServerUrl"],
    ["phase-server.refusePunycodeHost", "previewSiteUrl"],
    ["phase-server.validateDefaultServerUrl", "defaultMultiplayerServerUrl"],
    ["phase-server.validatePreviewSiteUrl", "previewSiteUrl"],
  ])("stops at %s defined again in another template file, for web.%s", (name, key) => {
    const copy = new Map(templates).set(other, `{{- define "${name}" -}}x{{- end -}}\n`);
    expect(() => chartAccepts(key, copy)).toThrow(
      `template "${name}" is defined 2 times among the chart's templates, in ${HELPERS} and ${other}`,
    );
  });
});
