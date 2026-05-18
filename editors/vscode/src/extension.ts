import * as path from "node:path";
import * as fs from "node:fs";
import {
  ExtensionContext,
  workspace,
  window,
  commands,
  WorkspaceConfiguration,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
  context.subscriptions.push(
    commands.registerCommand("typhon.restartServer", () => restart(context)),
  );

  await start(context);
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

async function start(context: ExtensionContext): Promise<void> {
  const config = workspace.getConfiguration("typhon");
  if (!config.get<boolean>("server.enable", true)) {
    return;
  }

  const command = resolveCommand(config);
  if (!command) {
    window.showWarningMessage(
      "Typhon: `tyc` binary not found. Set `typhon.server.path` or add `tyc` to PATH. " +
        "Syntax highlighting will still work without the language server.",
    );
    return;
  }

  const args = config.get<string[]>("server.arguments", ["lsp"]);

  const serverOptions: ServerOptions = {
    run: { command, args, transport: TransportKind.stdio },
    debug: { command, args, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "typhon" },
      { scheme: "untitled", language: "typhon" },
    ],
    synchronize: {
      fileEvents: [
        workspace.createFileSystemWatcher("**/*.ty"),
        workspace.createFileSystemWatcher("**/*.dty"),
        workspace.createFileSystemWatcher("**/typhon.toml"),
      ],
    },
    outputChannelName: "Typhon Language Server",
  };

  client = new LanguageClient(
    "typhon",
    "Typhon Language Server",
    serverOptions,
    clientOptions,
  );

  try {
    await client.start();
    context.subscriptions.push(client);
  } catch (err) {
    window.showErrorMessage(`Typhon: failed to start \`tyc lsp\`: ${err}`);
    client = undefined;
  }
}

async function restart(context: ExtensionContext): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
  await start(context);
}

function resolveCommand(config: WorkspaceConfiguration): string | undefined {
  const configured = config.get<string>("server.path", "tyc");
  if (path.isAbsolute(configured)) {
    return fs.existsSync(configured) ? configured : undefined;
  }
  return configured;
}
