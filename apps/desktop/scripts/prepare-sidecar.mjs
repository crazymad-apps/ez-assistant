import { copyFile, mkdir } from "node:fs/promises";
import { arch, platform } from "node:process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const targetTriple = resolveTargetTriple(platform, arch);
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(scriptDirectory, "../../..");
const extension = platform === "win32" ? ".exe" : "";
const source = resolve(workspaceRoot, `target/release/ez-assistant-runtime${extension}`);
const outputDirectory = resolve(scriptDirectory, "../src-tauri/binaries");
const destination = resolve(
  outputDirectory,
  `ez-assistant-runtime-${targetTriple}${extension}`,
);

await mkdir(outputDirectory, { recursive: true });
await copyFile(source, destination);

function resolveTargetTriple(currentPlatform, currentArch) {
  if (currentPlatform === "darwin" && currentArch === "arm64") {
    return "aarch64-apple-darwin";
  }
  if (currentPlatform === "darwin" && currentArch === "x64") {
    return "x86_64-apple-darwin";
  }
  if (currentPlatform === "linux" && currentArch === "x64") {
    return "x86_64-unknown-linux-gnu";
  }
  throw new Error(`Sidecar packaging is not configured for ${currentPlatform}/${currentArch}`);
}
