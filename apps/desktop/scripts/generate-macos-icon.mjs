import { spawn } from "node:child_process";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { platform } from "node:process";
import { fileURLToPath } from "node:url";

if (platform !== "darwin") {
  throw new Error("The macOS ICNS generator can only run on macOS.");
}

const checkOnly = process.argv.slice(2).includes("--check");
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const desktopDirectory = resolve(scriptDirectory, "..");
const sourceIcon = resolve(desktopDirectory, "app-icon.macos.svg");
const destinationIcon = resolve(desktopDirectory, "src-tauri/icons/macos/icon.icns");
const tauriExecutable = resolve(desktopDirectory, "node_modules/.bin/tauri");
const temporaryDirectory = await mkdtemp(join(tmpdir(), "ez-assistant-macos-icon-"));

try {
  await run(tauriExecutable, ["icon", sourceIcon, "--output", temporaryDirectory]);

  const generatedIcon = resolve(temporaryDirectory, "icon.icns");
  const generatedMetadata = await stat(generatedIcon);
  if (generatedMetadata.size === 0) {
    throw new Error("Tauri generated an empty macOS icon.");
  }

  if (checkOnly) {
    await compareIconRepresentations(generatedIcon, destinationIcon);
    console.log("macOS icon is up to date.");
  } else {
    await mkdir(dirname(destinationIcon), { recursive: true });
    await copyFile(generatedIcon, destinationIcon);
    console.log(`Generated macOS icon: ${destinationIcon}`);
  }
} finally {
  await rm(temporaryDirectory, { force: true, recursive: true });
}

async function compareIconRepresentations(generatedIcon, committedIcon) {
  const generatedIconset = resolve(temporaryDirectory, "generated.iconset");
  const committedIconset = resolve(temporaryDirectory, "committed.iconset");
  await run("iconutil", ["-c", "iconset", generatedIcon, "-o", generatedIconset]);
  await run("iconutil", ["-c", "iconset", committedIcon, "-o", committedIconset]);

  const [generatedFiles, committedFiles] = await Promise.all([
    readdir(generatedIconset),
    readdir(committedIconset),
  ]);
  generatedFiles.sort();
  committedFiles.sort();
  if (JSON.stringify(generatedFiles) !== JSON.stringify(committedFiles)) {
    throwStaleIconError();
  }

  for (const filename of generatedFiles) {
    const [generatedBytes, committedBytes] = await Promise.all([
      readFile(resolve(generatedIconset, filename)),
      readFile(resolve(committedIconset, filename)),
    ]);
    if (!generatedBytes.equals(committedBytes)) {
      throwStaleIconError();
    }
  }
}

function throwStaleIconError() {
  throw new Error(
    "The committed macOS icon is stale. Run `npm run generate:icon:macos`.",
  );
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
