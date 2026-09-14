/**
 * Unit tests for the scenario sniff, run by `node --test` (see the
 * `test` script). They need no VS Code host because `scenario.ts`
 * imports nothing from `vscode`.
 *
 * The positive cases read the real files under `examples/fight/`, so the
 * sniff cannot drift away from the scenarios the repo actually ships.
 * `repoRoot()` throws rather than letting those cases silently vanish if
 * the compiled output moves.
 */

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';

import { scenarioLenses } from './scenario';

/** Walk up from the compiled test until the examples directory shows up. */
function repoRoot(): string {
  let dir = __dirname;
  for (let i = 0; i < 8; i++) {
    if (fs.existsSync(path.join(dir, 'examples', 'fight', 'duel.toml'))) {
      return dir;
    }
    const parent = path.dirname(dir);
    if (parent === dir) {
      break;
    }
    dir = parent;
  }
  throw new Error(`no examples/fight/duel.toml above ${__dirname}`);
}

function fixture(...parts: string[]): string {
  const file = path.join(repoRoot(), ...parts);
  assert.ok(fs.existsSync(file), `missing fixture ${file}`);
  return fs.readFileSync(file, 'utf8');
}

test('duel.toml gets fight + matrix + one debug lens per AI', () => {
  const lenses = scenarioLenses(fixture('examples', 'fight', 'duel.toml'));
  assert.deepEqual(
    lenses.filter((l) => l.kind !== 'debug'),
    [
      { line: 0, kind: 'fight' },
      { line: 0, kind: 'matrix' },
    ],
  );
  const debug = lenses.filter((l) => l.kind === 'debug');
  assert.deepEqual(
    debug.map((l) => ({ ai: l.ai, entityId: l.entityId })),
    [
      { ai: 'ais/hero.leek', entityId: 1 },
      { ai: 'ais/villain.leek', entityId: 2 },
    ],
  );
  // The lens sits on the `ai = "…"` line, not at the top of the file.
  assert.ok(debug.every((l) => l.line > 0));
});

test('skirmish.toml has no [testing] table, so no matrix lens', () => {
  const lenses = scenarioLenses(fixture('examples', 'fight', 'skirmish.toml'));
  assert.deepEqual(
    lenses.map((l) => l.kind),
    ['fight', 'debug', 'debug'],
  );
  assert.deepEqual(
    lenses.filter((l) => l.kind === 'debug').map((l) => l.entityId),
    [1, 2],
  );
});

test('a scenario fragment with no entities and no extends gets nothing', () => {
  // base-arena.toml is the sharp case: a real scenario file with a map
  // and a seed, but nothing to fight with. Running it is meaningless.
  assert.deepEqual(scenarioLenses(fixture('examples', 'fight', 'base-arena.toml')), []);
});

test('project manifests and unrelated TOML get nothing', () => {
  for (const file of [
    ['examples', 'fight', 'Miku.toml'],
    ['Cargo.toml'],
    ['deny.toml'],
    ['rust-toolchain.toml'],
  ]) {
    assert.deepEqual(scenarioLenses(fixture(...file)), [], file.join('/'));
  }
});

test('an entity whose id follows its ai still carries the id', () => {
  const lenses = scenarioLenses('[[entities]]\nai = "a.leek"\nid = 7\n');
  assert.deepEqual(lenses, [
    { line: 0, kind: 'fight' },
    { line: 1, kind: 'debug', ai: 'a.leek', entityId: 7 },
  ]);
});

test('an entity with no explicit ai gets no debug lens', () => {
  // `leek = "…"` indirection and `extends` inheritance both mean the ai
  // may not appear in this file. A missing lens is the right answer.
  const text = 'extends = "base.toml"\n\n[[entities]]\nid = 1\nleek = "leeks/hero.toml"\n';
  assert.deepEqual(scenarioLenses(text), [{ line: 0, kind: 'fight' }]);
});

test('profile entity overrides are not debuggable', () => {
  // Debugging one needs `--profile aggressive` applied first, which this
  // lens does not do — so it must not offer to.
  const text =
    '[[entities]]\nid = 1\nai = "a.leek"\n\n[[profiles.aggressive.entities]]\nid = 1\nai = "b.leek"\n';
  assert.deepEqual(
    scenarioLenses(text)
      .filter((l) => l.kind === 'debug')
      .map((l) => l.ai),
    ['a.leek'],
  );
});

test('a commented-out entity table does not make a file a scenario', () => {
  assert.deepEqual(scenarioLenses('# [[entities]]\n# ai = "a.leek"\n'), []);
});

test('extends only counts at the top level', () => {
  // A key named `extends` inside some other table is not scenario
  // inheritance.
  assert.deepEqual(scenarioLenses('[tool.something]\nextends = "other"\n'), []);
});

test('a # inside a quoted ai path is not a comment', () => {
  const lenses = scenarioLenses('[[entities]]\nid = 3\nai = "ais/a#b.leek"  # the hero\n');
  assert.deepEqual(
    lenses.filter((l) => l.kind === 'debug'),
    [{ line: 2, kind: 'debug', ai: 'ais/a#b.leek', entityId: 3 }],
  );
});

test('an oversized file is not scanned', () => {
  const huge = '[[entities]]\nai = "a.leek"\n' + 'x'.repeat(300 * 1024);
  assert.deepEqual(scenarioLenses(huge), []);
});
