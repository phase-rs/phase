#!/usr/bin/env node
// Read-only scout: a cheap agent gathers located facts for a planner or critic.
//   verify  checks a scout agent's JSON-lines report against the repository and prints the fact pack.
//   run     launches the scout from the command line when the orchestrator has no native sub-agent:
//           GPT-6 Luna through `codex exec` when codex is installed, otherwise Sonnet 5.5 through `claude -p`.
// Only facts whose quote is the whole line at (or within a few lines of) the cited location are kept.
// A launched scout that has not finished after --timeout seconds (default one hour) is stopped and reported as failed.
import { spawn, spawnSync } from 'node:child_process';
import { appendFileSync, readFileSync } from 'node:fs';
import { isAbsolute, relative, resolve } from 'node:path';
import { parseArgs } from 'node:util';

const CONTRACT = readFileSync(new URL('../references/scout-contract.md', import.meta.url), 'utf8');
const LINE_SLACK = 3;
const BACKENDS = {
  codex: {
    model: 'gpt-6-luna',
    args: ({ repo, model, effort }) => [
      'exec', '--ignore-user-config', '--ephemeral', '--skip-git-repo-check', '-s', 'read-only',
      '-m', model, '-c', `model_reasoning_effort="${effort}"`, '-C', repo, '--json', '-',
    ],
    prompt: task => `${CONTRACT}\n${task}`,
  },
  claude: {
    model: 'claude-sonnet-5-5',
    args: ({ model, effort }) => [
      '-p', '--model', model, '--effort', effort, '--tools', 'Read,Grep,Glob', '--strict-mcp-config',
      '--no-session-persistence', '--append-system-prompt', CONTRACT, '--output-format', 'stream-json', '--verbose',
    ],
    prompt: task => task,
  },
};

const { positionals: [command], values } = parseArgs({
  allowPositionals: true,
  options: {
    repo: { type: 'string', default: process.cwd() },
    'task-file': { type: 'string' },
    'report-file': { type: 'string' },
    backend: { type: 'string' },
    model: { type: 'string' },
    effort: { type: 'string', default: 'low' },
    timeout: { type: 'string', default: '3600' },
    label: { type: 'string', default: '' },
  },
});
const repo = resolve(values.repo);

if (command === 'verify' && values['report-file']) {
  emit(readFileSync(values['report-file'], 'utf8'), {});
} else if (command === 'run' && values['task-file']) {
  emit(...await launch(readFileSync(values['task-file'], 'utf8')));
} else {
  console.error(`usage: scout.mjs verify --report-file FILE [--repo DIR] [--label L]
       scout.mjs run --task-file FILE [--repo DIR] [--backend codex|claude] [--model ID] [--effort LEVEL] [--timeout SECONDS] [--label L]`);
  process.exit(2);
}

async function launch(task) {
  const name = values.backend ?? (spawnSync('codex', ['--version']).error ? 'claude' : 'codex');
  const backend = BACKENDS[name];
  if (!backend) fail(`unknown backend ${name}`);
  const model = values.model ?? backend.model;
  const started = Date.now();
  const child = spawn(name, backend.args({ repo, model, effort: values.effort }), {
    cwd: repo,
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  child.stdin.end(backend.prompt(task));
  // Exit here rather than wait for `close`: a descendant holding the pipe open would keep it from firing.
  const deadline = setTimeout(() => {
    child.kill('SIGKILL');
    fail(`${name} did not finish within ${values.timeout} seconds`);
  }, Number(values.timeout) * 1000);

  const usage = { input: 0, output: 0, cacheRead: 0 };
  let report = '';
  let pending = '';
  const onRecord = record => {
    if (!record.trim()) return;
    const event = JSON.parse(record);
    if (event.type === 'item.completed' && event.item.type === 'agent_message') report = event.item.text;
    if (event.type === 'turn.completed') {
      const { input_tokens: input, cached_input_tokens: cached, output_tokens: output } = event.usage;
      usage.input += input - cached;
      usage.cacheRead += cached;
      usage.output += output;
    }
    if (event.type === 'result') {
      report = event.result;
      usage.input += event.usage.input_tokens + event.usage.cache_creation_input_tokens;
      usage.cacheRead += event.usage.cache_read_input_tokens;
      usage.output += event.usage.output_tokens;
    }
  };
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    pending += chunk;
    const records = pending.split('\n');
    pending = records.pop();
    for (const record of records) onRecord(record.replace(/\r$/, ''));
  });
  const exitCode = await new Promise(done => child.on('close', done));
  clearTimeout(deadline);
  onRecord(pending);
  if (exitCode !== 0) fail(`${name} exited with ${exitCode}`);
  return [report, { backend: name, model, usage, durationMs: Date.now() - started }];
}

function emit(report, run) {
  const facts = [], rejected = [], unknowns = [];
  let malformed = 0;
  for (const line of report.split('\n')) {
    if (!line.trim().startsWith('{')) continue;
    let record;
    try {
      record = JSON.parse(line);
    } catch {
      malformed++;
      continue;
    }
    if ('unknown' in record) {
      unknowns.push(record.unknown);
      continue;
    }
    const reason = verify(record);
    (reason ? rejected : facts).push(reason ? { ...record, reason } : record);
  }
  if (!facts.length) fail(`scout returned no verified facts (${malformed} malformed lines)`);
  console.log(JSON.stringify({ ...run, facts, rejected, unknowns, malformed }, null, 2));
  if (process.env.SCOUT_USAGE_LOG) {
    appendFileSync(process.env.SCOUT_USAGE_LOG, `${JSON.stringify({
      at: new Date().toISOString(), label: values.label, ...run, facts: facts.length, rejected: rejected.length, malformed,
    })}\n`);
  }
}

// Returns a rejection reason, or nothing when the quote is found; corrects an off-by-a-few line number in place.
function verify(fact) {
  const quote = String(fact.quote ?? '').trim();
  if (!quote) return 'empty quote';
  const path = resolve(repo, String(fact.path ?? ''));
  const inside = relative(repo, path);
  if (!inside || inside.startsWith('..') || isAbsolute(inside)) return 'path outside repository';
  let lines;
  try {
    lines = readFileSync(path, 'utf8').split('\n');
  } catch {
    return 'file not readable';
  }
  const cited = Number(fact.line) - 1;
  for (let offset = 0; offset <= LINE_SLACK; offset++) {
    for (const index of new Set([cited - offset, cited + offset])) {
      if (lines[index]?.trim() === quote) {
        fact.line = index + 1;
        return undefined;
      }
    }
  }
  return 'quote not at cited line';
}

function fail(message) {
  console.error(`scout: ${message}`);
  process.exit(1);
}
