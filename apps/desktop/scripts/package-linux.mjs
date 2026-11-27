// Build local Ubuntu / Debian x86_64 .deb release artifact and verify its contents.
import "./check-version.mjs";
import { createHash } from "node:crypto";
import { execFileSync, spawn } from "node:child_process";
import { copyFile, mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

if (process.platform !== "linux") {
  throw new Error("Linux .deb packaging must run on Linux (native Ubuntu 24.04 or linux/amd64 container).");
}

const desktopRoot = fileURLToPath(new URL("../", import.meta.url));
const workspaceRoot = resolve(desktopRoot, "../..");
const targetRoot = resolve(workspaceRoot, process.env.CARGO_TARGET_DIR ?? "target");
const { version } = JSON.parse(await readFile(resolve(desktopRoot, "src-tauri/tauri.conf.json"), "utf8"));
const arch = process.arch === "arm64" ? "arm64" : "amd64";
const started = Date.now();

await run(process.execPath, ["scripts/run-tauri.mjs", "build", "--bundles", "deb"]);

const debDirectory = resolve(targetRoot, "release/bundle/deb");
const debEntries = (await readdir(debDirectory)).filter((name) => name.endsWith(".deb"));
if (debEntries.length === 0) {
  throw new Error(`No .deb artifact found under ${debDirectory}`);
}

let source = "";
let latestMtime = 0;
for (const entry of debEntries) {
  const candidate = resolve(debDirectory, entry);
  const info = await stat(candidate);
  if (info.mtimeMs >= latestMtime) {
    latestMtime = info.mtimeMs;
    source = candidate;
  }
}
if (!source || latestMtime < started - 2000) {
  throw new Error(`Refusing stale .deb artifact: ${source || debDirectory}`);
}

const listing = execFileSync("dpkg-deb", ["-c", source], { encoding: "utf8" });
for (const expected of [
  "usr/bin/ez-assistant-desktop",
  "usr/bin/ez-assistant-runtime",
  "runtime-tools/bin/rg",
  "usr/share/applications/",
  ".desktop",
]) {
  if (!listing.includes(expected)) {
    throw new Error(`Missing expected entry "${expected}" in ${source}`);
  }
}

const outputDirectory = resolve(targetRoot, "linux-release", version);
await mkdir(outputDirectory, { recursive: true });
const filename = `EZ-Assistant_${version}_linux_${arch}.deb`;
const destination = resolve(outputDirectory, filename);
await copyFile(source, destination);

const bytes = await readFile(destination);
const digest = createHash("sha256").update(bytes).digest("hex");
await writeFile(resolve(outputDirectory, `${filename}.sha256`), `${digest}  ${filename}\n`);

console.log(`Created ${destination} (${(bytes.length / (1024 * 1024)).toFixed(2)} MiB, SHA-256: ${digest})`);

function run(command, arguments_) {
  return new Promise((accept, reject) => {
    const child = spawn(command, arguments_, { cwd: desktopRoot, env: process.env, stdio: "inherit" });
    child.once("error", reject);
    child.once("exit", (code) => (code === 0 ? accept() : reject(new Error(`${command} failed: ${code}`))));
  });
}
