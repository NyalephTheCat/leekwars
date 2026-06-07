import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node';

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
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.leek'),
    },
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

export function activate(context: vscode.ExtensionContext) {
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

  void startServer();
}

export function deactivate(): Thenable<void> | undefined {
  return client ? client.stop() : undefined;
}
