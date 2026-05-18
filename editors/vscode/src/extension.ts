import * as path from "node:path";
import * as fs from "node:fs";
import * as os from "node:os";
import {
  ExtensionContext,
  workspace,
  window,
  commands,
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
    commands.registerCommand("typhon.restartServer", () => restart()),
  );

  await start();
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

async function start(): Promise<void> {
  const config = workspace.getConfiguration("typhon");
  if (!config.get<boolean>("server.enable", true)) {
    return;
  }

  const configured = config.get<string>("server.path", "tyc");
  const command = resolveCommand(configured);
  if (!command) {
    window.showWarningMessage(
      `Typhon: \`${configured}\` not found. Set \`typhon.server.path\` or add \`tyc\` to PATH. ` +
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
  } catch (err) {
    window.showErrorMessage(
      `Typhon: failed to start \`${command} ${args.join(" ")}\`: ${err}`,
    );
    client = undefined;
  }
}

async function restart(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
  await start();
}

function resolveCommand(configured: string): string | undefined {
  if (path.isAbsolute(configured)) {
    return fs.existsSync(configured) ? configured : undefined;
  }
  if (configured.includes(path.sep) || configured.includes("/")) {
    const resolved = path.resolve(configured);
    return fs.existsSync(resolved) ? resolved : undefined;
  }
  return findOnPath(configured);
}

function findOnPath(name: string): string | undefined {
  const pathEnv = process.env.PATH;
  if (!pathEnv) {
    return undefined;
  }
  const isWindows = os.platform() === "win32";
  const extensions = isWindows
    ? (process.env.PATHEXT ?? ".COM;.EXE;.BAT;.CMD").split(";")
    : [""];
  for (const dir of pathEnv.split(path.delimiter)) {
    if (!dir) continue;
    for (const ext of extensions) {
      const candidate = path.join(dir, name + ext);
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }
  }
  return undefined;
}
