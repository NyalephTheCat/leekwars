import * as path from 'node:path';

import * as vscode from 'vscode';
import {
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  Location as LspLocation,
  Position as LspPosition,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node';

import { scenarioLenses } from './scenario';

let client: LanguageClient | undefined;

/** Build a fresh client from the current `leek.*` configuration. */
function buildClient(): LanguageClient {
  const config = vscode.workspace.getConfiguration('leek');
  const serverPath = config.get<string>('server.path') || 'leek-lsp';

  const serverOptions: ServerOptions = {
    run: { command: serverPath, transport: TransportKind.stdio },
    debug: { command: serverPath, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'leek' }],
    // No `synchronize.fileEvents` watcher here: the server registers its
    // own `**/*.leek` watcher dynamically in `initialized`, and having
    // both made VS Code report every disk change twice.
    // Host-environment function libraries (e.g. "leekwars" for the
    // leek-wars-generator fight functions, or a path to a .lib file). The
    // server registers their functions so they aren't flagged as undefined.
    initializationOptions: {
      libraries: config.get<string[]>('libraries') ?? [],
    },
  };

  return new LanguageClient('leek', 'Leekscript', serverOptions, clientOptions);
}

async function startServer(): Promise<void> {
  client = buildClient();
  await client.start();
}

async function stopServer(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

/** Stop the running server (if any) and start a fresh one — picks up a
 *  rebuilt `leek-lsp` binary or changed `leek.server.path` without
 *  reloading the whole window. */
async function restartServer(): Promise<void> {
  await stopServer();
  await startServer();
}

/** Launches the `leek-dap` debug adapter as a stdio subprocess. The path
 *  comes from `leek.debugAdapter.path` (default `leek-dap` on PATH). */
class LeekDebugAdapterFactory implements vscode.DebugAdapterDescriptorFactory {
  createDebugAdapterDescriptor(
    _session: vscode.DebugSession,
  ): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    const config = vscode.workspace.getConfiguration('leek');
    const adapterPath = config.get<string>('debugAdapter.path') || 'leek-dap';
    return new vscode.DebugAdapterExecutable(adapterPath, []);
  }
}

/** Fills in a default `launch` config when the user hits F5 with no
 *  launch.json (debugs the active `.leek` file). */
class LeekDebugConfigurationProvider implements vscode.DebugConfigurationProvider {
  resolveDebugConfiguration(
    _folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): vscode.ProviderResult<vscode.DebugConfiguration> {
    if (!config.type && !config.request && !config.name) {
      const editor = vscode.window.activeTextEditor;
      if (editor?.document.languageId === 'leek') {
        config.type = 'leek';
        config.name = 'Debug Leekscript file';
        config.request = 'launch';
        config.program = '${file}';
        config.stopOnEntry = false;
      }
    }
    if (!config.program) {
      void vscode.window.showErrorMessage('Leekscript debug: no `program` to launch.');
      return undefined;
    }
    return config;
  }
}

// --- Running fights and tests -------------------------------------------
//
// Every command below is registered *client-side*, and none of them may
// ever be added to `execute_command::COMMANDS` on the server:
// vscode-languageclient auto-registers a proxy for each advertised
// command, and VS Code throws on a duplicate id at activation — taking
// the language client and the debug adapter down with it, not just the
// one command. `crates/tools/leek-lsp/tests/handlers.rs` pins that list
// so the rule survives a well-meaning future edit.

/** Extra `--library` flags for a `miku` run, from the same
 *  `leek.libraries` setting the server gets. The fight builtins live in
 *  a library ("leekwars"), so without this a fight over an AI that calls
 *  them fails to compile — exactly as it would on the command line. */
function libraryArgs(): string[] {
  const libs = vscode.workspace.getConfiguration('leek').get<string[]>('libraries') ?? [];
  return libs.flatMap((lib) => ['--library', lib]);
}

/** The folder a run happens in: the one owning `file`, else the first
 *  workspace folder so a single-root workspace works from the palette. */
function folderFor(file: vscode.Uri): vscode.WorkspaceFolder | undefined {
  return vscode.workspace.getWorkspaceFolder(file) ?? vscode.workspace.workspaceFolders?.[0];
}

/** The scenario a command applies to: the path a code lens passed, the
 *  `Uri` an editor-title menu passed, else the active editor's file. */
function scenarioArg(arg: unknown): string | undefined {
  if (typeof arg === 'string') {
    return arg;
  }
  if (arg instanceof vscode.Uri) {
    return arg.fsPath;
  }
  const doc = vscode.window.activeTextEditor?.document;
  return doc?.uri.scheme === 'file' ? doc.uri.fsPath : undefined;
}

/** Run `miku` as a VS Code task rather than a child process. A matrix
 *  sweep plays its cells one after another (`[fight].jobs` is parsed but
 *  not yet honoured, #134), so a click can take minutes: the user needs
 *  a terminal they can read and cancel, not a spinner. */
async function runMiku(
  name: string,
  args: string[],
  folder: vscode.WorkspaceFolder | undefined,
  problemMatchers: string[] = [],
): Promise<void> {
  const mikuPath = vscode.workspace.getConfiguration('leek').get<string>('miku.path') || 'miku';
  const task = new vscode.Task(
    { type: 'leek', command: name },
    folder ?? vscode.TaskScope.Workspace,
    name,
    'leek',
    new vscode.ProcessExecution(mikuPath, args, { cwd: folder?.uri.fsPath }),
    problemMatchers,
  );
  task.group = vscode.TaskGroup.Test;
  task.presentationOptions = {
    reveal: vscode.TaskRevealKind.Always,
    panel: vscode.TaskPanelKind.Dedicated,
    clear: true,
  };
  try {
    await vscode.tasks.executeTask(task);
  } catch (err) {
    // The extension never installs `miku`; say so instead of letting the
    // task fail with an opaque spawn error.
    void vscode.window.showErrorMessage(
      `Leekscript: could not run \`${mikuPath}\` — ${err}. ` +
        'Set `leek.miku.path` if miku is not on PATH.',
    );
  }
}

/** `miku fight <scenario>`, or `--mode matrix` for the sweep. */
async function runFight(arg: unknown, mode?: 'matrix'): Promise<void> {
  const scenario = scenarioArg(arg);
  if (!scenario) {
    void vscode.window.showErrorMessage('Leekscript: no scenario file to run.');
    return;
  }
  const args = [...libraryArgs(), 'fight', scenario];
  if (mode) {
    args.push('--mode', mode);
  }
  const label = `${mode ?? 'fight'} ${path.basename(scenario)}`;
  await runMiku(label, args, folderFor(vscode.Uri.file(scenario)));
}

/** `miku test` over the project's `[paths].tests`. The problem matcher
 *  turns each `FAIL <path> — <reason>` line into a diagnostic; there is
 *  no Test Explorer here because `miku test` has no machine-readable
 *  result stream yet (only JUnit XML, written as a whole file at the
 *  end) and scraping stdout into one would be a stub. */
async function runTests(): Promise<void> {
  const folder = vscode.window.activeTextEditor
    ? folderFor(vscode.window.activeTextEditor.document.uri)
    : vscode.workspace.workspaceFolders?.[0];
  await runMiku('test', [...libraryArgs(), 'test'], folder, ['$miku-test']);
}

/** Start a fight-debug session for one entity of a scenario — what
 *  `examples/fight/.vscode/launch.json` spells out by hand. */
async function debugFightHere(scenarioPath: unknown, ai: unknown, entityId: unknown): Promise<void> {
  const scenario = scenarioArg(scenarioPath);
  if (!scenario || typeof ai !== 'string') {
    void vscode.window.showErrorMessage('Leekscript: no scenario and AI to debug.');
    return;
  }
  const config: vscode.DebugConfiguration = {
    type: 'leek',
    request: 'launch',
    name: `Debug ${path.basename(ai)} in ${path.basename(scenario)}`,
    // `ai` is relative to the scenario file, the way the scenario loader
    // resolves it.
    program: path.resolve(path.dirname(scenario), ai),
    scenario,
    stopOnEntry: true,
  };
  if (typeof entityId === 'number') {
    // Optional: without it the adapter picks the entity whose ai is this
    // program, else the first one.
    config.fightEntity = entityId;
  }
  const started = await vscode.debug.startDebugging(folderFor(vscode.Uri.file(scenario)), config);
  if (!started) {
    void vscode.window.showErrorMessage(`Leekscript: could not start debugging ${ai}.`);
  }
}

/** One row of the server's `leek.analyze` answer. */
interface ComplexityRecord {
  name: string;
  params: string[];
  big_o: string;
  formula: string;
}

/** Ask the server for the whole document's complexity records and show
 *  them. Named `leek.showAnalysis`, *not* `leek.analyze`: that id is the
 *  server's and claiming it here would be the duplicate-registration
 *  collision described above. */
async function showAnalysis(): Promise<void> {
  const doc = vscode.window.activeTextEditor?.document;
  if (doc?.languageId !== 'leek') {
    void vscode.window.showErrorMessage('Leekscript: open a .leek file to analyze.');
    return;
  }
  if (!client) {
    void vscode.window.showErrorMessage('Leekscript: the language server is not running.');
    return;
  }
  try {
    const records = (await client.sendRequest(ExecuteCommandRequest.type, {
      command: 'leek.analyze',
      arguments: [doc.uri.toString()],
    })) as ComplexityRecord[] | null;
    if (!records?.length) {
      void vscode.window.showInformationMessage('Leekscript: no user functions to analyze.');
      return;
    }
    await vscode.window.showQuickPick(
      records.map((r) => ({
        label: `${r.name}(${r.params.join(', ')})`,
        description: r.big_o,
        detail: `ops: ${r.formula}`,
      })),
      { title: `Complexity — ${path.basename(doc.uri.fsPath)}`, matchOnDetail: true },
    );
  } catch (err) {
    void vscode.window.showErrorMessage(`Leekscript: analysis failed — ${err}`);
  }
}

/** Draws the run/debug lenses over fight scenarios. Client-side for the
 *  same reason `leek.showReferences` is: the server's document selector
 *  is `.leek` only, so it never sees a scenario file, and it carries no
 *  scenario parser. [`scenarioLenses`] decides *what* to draw and is
 *  unit-tested; this only maps the result onto commands. */
class ScenarioCodeLensProvider implements vscode.CodeLensProvider {
  provideCodeLenses(document: vscode.TextDocument): vscode.CodeLens[] {
    const file = document.uri.fsPath;
    return scenarioLenses(document.getText()).map((lens) => {
      const range = new vscode.Range(lens.line, 0, lens.line, 0);
      const command: vscode.Command =
        lens.kind === 'debug'
          ? {
              title: `Debug ${lens.ai} in this fight`,
              command: 'leek.debugFightHere',
              arguments: [file, lens.ai, lens.entityId],
            }
          : lens.kind === 'matrix'
            ? { title: 'Run matrix', command: 'leek.runFightMatrix', arguments: [file] }
            : { title: 'Run fight', command: 'leek.runFight', arguments: [file] };
      return new vscode.CodeLens(range, command);
    });
  }
}

export function activate(context: vscode.ExtensionContext) {
  // The "N references" code lens resolves to this command. It is
  // client-side on purpose — only the editor can open a peek view — and
  // the server deliberately does not advertise it in
  // `executeCommandProvider`, so nothing else claims the id.
  context.subscriptions.push(
    vscode.commands.registerCommand(
      'leek.showReferences',
      (uri: string, position: LspPosition, locations: LspLocation[]) => {
        const converter = client?.protocol2CodeConverter;
        if (!converter) {
          return;
        }
        void vscode.commands.executeCommand(
          'editor.action.showReferences',
          vscode.Uri.parse(uri),
          converter.asPosition(position),
          locations.map((l) => converter.asLocation(l)),
        );
      },
    ),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('leek.restartServer', async () => {
      try {
        await restartServer();
        vscode.window.showInformationMessage('Leekscript: language server restarted.');
      } catch (err) {
        vscode.window.showErrorMessage(`Leekscript: failed to restart server — ${err}`);
      }
    }),
  );

  // Debugging: register the `leek` debug type and its adapter factory.
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory('leek', new LeekDebugAdapterFactory()),
    vscode.debug.registerDebugConfigurationProvider('leek', new LeekDebugConfigurationProvider()),
  );

  // Running fights and tests. All client-side — see the note above
  // `libraryArgs`.
  context.subscriptions.push(
    vscode.commands.registerCommand('leek.runFight', (arg: unknown) => runFight(arg)),
    vscode.commands.registerCommand('leek.runFightMatrix', (arg: unknown) => runFight(arg, 'matrix')),
    vscode.commands.registerCommand('leek.runTests', () => runTests()),
    vscode.commands.registerCommand('leek.debugFightHere', debugFightHere),
    vscode.commands.registerCommand('leek.showAnalysis', () => showAnalysis()),
    vscode.languages.registerCodeLensProvider(
      { scheme: 'file', pattern: '**/*.toml' },
      new ScenarioCodeLensProvider(),
    ),
  );

  void startServer();
}

export function deactivate(): Thenable<void> | undefined {
  return client ? client.stop() : undefined;
}
