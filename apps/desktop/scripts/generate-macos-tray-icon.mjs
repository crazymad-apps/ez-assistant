import { spawn } from "node:child_process";
import {
  copyFile,
  mkdtemp,
  readFile,
  rm,
  stat,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { platform } from "node:process";
import { fileURLToPath } from "node:url";

if (platform !== "darwin") {
  throw new Error("The macOS tray icon generator can only run on macOS.");
}

const checkOnly = process.argv.slice(2).includes("--check");
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const desktopDirectory = resolve(scriptDirectory, "..");
const sourceIcon = resolve(desktopDirectory, "tray-icon.macos.svg");
const destinationIcon = resolve(desktopDirectory, "src-tauri/icons/macos/tray-icon.png");
const tauriExecutable = resolve(desktopDirectory, "node_modules/.bin/tauri");
const temporaryDirectory = await mkdtemp(join(tmpdir(), "ez-assistant-tray-icon-"));

try {
  await run(tauriExecutable, ["icon", sourceIcon, "--output", temporaryDirectory]);

  // A 128 px source stays crisp when AppKit scales it to the 18 pt status-bar height.
  const generatedIcon = resolve(temporaryDirectory, "128x128.png");
  const generatedMetadata = await stat(generatedIcon);
  if (generatedMetadata.size === 0) {
    throw new Error("Tauri generated an empty macOS tray icon.");
  }

  if (checkOnly) {
    const [generatedBytes, committedBytes] = await Promise.all([
      readFile(generatedIcon),
      readFile(destinationIcon),
    ]);
    if (!generatedBytes.equals(committedBytes)) {
      throw new Error(
        "The committed macOS tray icon is stale. Run `npm run generate:icon:tray:macos`.",
      );
    }
    console.log("macOS tray icon is up to date.");
  } else {
    await copyFile(generatedIcon, destinationIcon);
    console.log(`Generated macOS tray icon: ${destinationIcon}`);
  }
} finally {
  await rm(temporaryDirectory, { force: true, recursive: true });
}

function run(command, arguments_) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, arguments_, { shell: false, stdio: "inherit" });
    child.once("error", rejectPromise);
    child.once("exit", (code) => {
      if (code === 0) {
        resolvePromise();
      } else {
        rejectPromise(new Error(`${command} exited with ${code ?? "unknown"}`));
      }
    });
  });
}
