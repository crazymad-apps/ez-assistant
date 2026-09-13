import { copyFile, mkdir, open } from "node:fs/promises";
import { arch, platform } from "node:process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const arguments_ = process.argv.slice(2);
if (arguments_.length !== 0 && (arguments_.length !== 2 || arguments_[0] !== "--target")) {
  throw new Error("Usage: prepare-sidecar.mjs [--target <triple>]");
}
const explicitTarget = arguments_[1];
const targetTriple = explicitTarget ?? resolveTargetTriple(platform, arch);
const supportedTargets = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];
if (!supportedTargets.includes(targetTriple)) throw new Error(`Unsupported sidecar target: ${targetTriple}`);
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(scriptDirectory, "../../..");
const extension = targetTriple.endsWith("-windows-msvc") ? ".exe" : "";
const targetRoot = resolve(workspaceRoot, process.env.CARGO_TARGET_DIR ?? "target");
const source = resolve(targetRoot, explicitTarget ?? "", `release/ez-assistant-runtime${extension}`);
const outputDirectory = resolve(scriptDirectory, "../src-tauri/binaries");
const destination = resolve(
  outputDirectory,
  `ez-assistant-runtime-${targetTriple}${extension}`,
);

if (extension === ".exe") {
  const file = await open(source, "r");
  try {
    const header = Buffer.alloc(4096);
    const { bytesRead } = await file.read(header, 0, header.length, 0);
    const offset = header.readUInt32LE(0x3c);
    const machine = targetTriple.startsWith("aarch64-") ? 0xaa64 : 0x8664;
    if (bytesRead < 64 || header.toString("ascii", 0, 2) !== "MZ" || offset + 6 > bytesRead
        || header.readUInt32LE(offset) !== 0x4550 || header.readUInt16LE(offset + 4) !== machine) {
      throw new Error(`Host PE architecture does not match ${targetTriple}: ${source}`);
    }
  } finally {
    await file.close();
  }
}
await mkdir(outputDirectory, { recursive: true });
await copyFile(source, destination);

function resolveTargetTriple(currentPlatform, currentArch) {
  if (currentPlatform === "win32" && currentArch === "x64") return "x86_64-pc-windows-msvc";
  if (currentPlatform === "win32" && currentArch === "arm64") return "aarch64-pc-windows-msvc";
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
