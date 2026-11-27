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
    const hint = isRelativePath(configured)
      ? "is a relative path, which is not accepted: set `typhon.server.path` to an absolute path, " +
        "or to a bare command name such as `tyc` that is looked up on PATH. "
      : "not found. Set `typhon.server.path` to an absolute path or add `tyc` to PATH. ";
    window.showWarningMessage(
      `Typhon: \`${configured}\` ${hint}` +
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
  // A path with a separator must be absolute. A relative one would resolve
  // against the extension host's working directory, which is not a location
  // the user chose — it can be the opened folder, so `./tools/tyc` could end
  // up launching a binary that a cloned repository ships.
  if (isRelativePath(configured)) {
    return undefined;
  }
  return findOnPath(configured);
}

/** True for a non-absolute value that contains a path separator. */
function isRelativePath(configured: string): boolean {
  return (
    !path.isAbsolute(configured) &&
    (configured.includes("/") || configured.includes("\\"))
  );
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
