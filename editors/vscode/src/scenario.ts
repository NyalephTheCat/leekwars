/**
 * Fight-scenario sniffing for the TOML code lenses.
 *
 * Deliberately pure and free of any `vscode` import: the lens logic is
 * the part that can be confidently wrong (a "▶ Fight" lens on
 * `Cargo.toml` is worse than no lens at all), and it is the only part
 * CI can test — there is no VS Code test host in this repo, so anything
 * touching the `vscode` module is unreachable from `node --test`.
 *
 * The server never sees these files: the language client's document
 * selector is `{ scheme: 'file', language: 'leek' }`, and the LSP has no
 * dependency on `leek-scenario` or `leek-manifest`. So the lens is
 * produced client-side, from the text alone.
 *
 * This is a regex sniff, not a TOML parser, and it is tuned to miss
 * rather than to guess: a scenario that inherits its entities through
 * `extends`, or names its AI indirectly through `leek = "…"`, gets no
 * debug lens. The cost of a miss is a lens the user does not see; the
 * cost of a guess is a fight the user did not mean to run.
 */

/** What clicking the lens does. */
export type ScenarioLensKind = 'fight' | 'matrix' | 'debug';

export interface ScenarioLens {
  /** 0-based line the lens is drawn above. */
  line: number;
  kind: ScenarioLensKind;
  /** `debug` only: the entity's `ai` path, relative to the scenario file. */
  ai?: string;
  /** `debug` only: the entity's `id`, passed to the adapter as `fightEntity`. */
  entityId?: number;
}

/** Files above this are not scenarios; scanning them line by line on
 *  every editor repaint is not worth it. `examples/fight/duel.toml` is
 *  under 2 KiB. */
const MAX_SCANNED_BYTES = 256 * 1024;

/** `[table]` / `[[array.of.tables]]` — captures the dotted name and
 *  whether it was a double bracket. */
const TABLE_HEADER = /^\s*(\[\[?)\s*([A-Za-z0-9_.\-"']+)\s*\]\]?\s*$/;
const EXTENDS_KEY = /^\s*extends\s*=/;
const AI_KEY = /^\s*ai\s*=\s*["']([^"']+)["']/;
const ID_KEY = /^\s*id\s*=\s*(-?\d+)\s*$/;

/**
 * Drop a trailing `#` comment, leaving quoted `#` alone so
 * `ai = "ais/a#b.leek"` survives. TOML's literal/basic string
 * distinction does not matter here: neither kind spans a line.
 */
function stripComment(line: string): string {
  let quote: string | undefined;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (quote) {
      if (ch === quote) {
        quote = undefined;
      } else if (ch === '\\' && quote === '"') {
        i++;
      }
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '#') {
      return line.slice(0, i);
    }
  }
  return line;
}

/** One `[[entities]]` block's interesting keys, if it has any. */
interface EntityBlock {
  /** Line of the `ai = "…"` key — where the debug lens is drawn. */
  aiLine: number;
  ai: string;
  id?: number;
}

/**
 * Returns the lenses to draw over a TOML document, or `[]` when the text
 * is not a fight scenario.
 *
 * A file counts as a scenario when it declares `[[entities]]` or
 * top-level `extends = "…"` (the two shapes every scenario in
 * `examples/fight/` has). `base-arena.toml` has neither — it is a
 * fragment meant to be inherited, and running it as a fight would play
 * an empty arena — so it correctly gets nothing.
 *
 * - `fight` (line 0) whenever the file is a scenario.
 * - `matrix` (line 0) only when the file carries a `[testing]` table:
 *   without one every sweep axis is empty and `--mode matrix` degenerates
 *   to a single fight, which is a confusing thing to offer as a button.
 * - `debug` for each `[[entities]]` block with an explicit `ai = "…"`,
 *   carrying that block's `id` as the entity to attach to. Entities
 *   inside a `[[profiles.*.entities]]` override are skipped: debugging
 *   one would need the profile applied, which this lens does not do.
 */
export function scenarioLenses(text: string): ScenarioLens[] {
  if (text.length > MAX_SCANNED_BYTES) {
    return [];
  }

  const lines = text.split(/\r?\n/);
  let table: string | undefined; // dotted name of the table we are inside
  let inEntities = false;
  let hasEntities = false;
  let hasExtends = false;
  let hasTesting = false;
  let block: Partial<EntityBlock> | undefined;
  const blocks: EntityBlock[] = [];

  const flush = (): void => {
    if (block?.ai !== undefined && block.aiLine !== undefined) {
      blocks.push({ aiLine: block.aiLine, ai: block.ai, id: block.id });
    }
    block = undefined;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = stripComment(lines[i]);
    const header = TABLE_HEADER.exec(line);
    if (header) {
      flush();
      const [, bracket, name] = header;
      table = name;
      inEntities = bracket === '[[' && name === 'entities';
      if (inEntities) {
        hasEntities = true;
        block = {};
      }
      if (name === 'testing' || name.startsWith('testing.')) {
        hasTesting = true;
      }
      continue;
    }
    if (table === undefined && EXTENDS_KEY.test(line)) {
      hasExtends = true;
      continue;
    }
    if (!inEntities || !block) {
      continue;
    }
    const ai = AI_KEY.exec(line);
    if (ai && block.ai === undefined) {
      block.ai = ai[1];
      block.aiLine = i;
      continue;
    }
    const id = ID_KEY.exec(line);
    if (id && block.id === undefined) {
      block.id = Number(id[1]);
    }
  }
  flush();

  if (!hasEntities && !hasExtends) {
    return [];
  }

  const out: ScenarioLens[] = [{ line: 0, kind: 'fight' }];
  if (hasTesting) {
    out.push({ line: 0, kind: 'matrix' });
  }
  for (const b of blocks) {
    out.push({ line: b.aiLine, kind: 'debug', ai: b.ai, entityId: b.id });
  }
  return out;
}
