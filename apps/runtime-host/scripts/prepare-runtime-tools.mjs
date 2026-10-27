// 构建期装配随 Host 分发的工具；产品运行时不下载，也不修改系统安装。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execute = promisify(execFile);
const manifest = JSON.parse(await readFile(new URL("./ripgrep-manifest.json", import.meta.url), "utf8"));
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function nativeTarget() {
  const cpu = { arm64: "aarch64", x64: "x86_64" }[process.arch];
  const os = { darwin: "apple-darwin", linux: "unknown-linux-gnu", win32: "pc-windows-msvc" }[process.platform];
  assert(cpu && os, `Unsupported runtime tools platform: ${process.platform}/${process.arch}`);
  return `${cpu}-${os}`;
}

export async function prepareRuntimeTools(directory, target, cacheRoot) {
  const artifactTarget = target === "x86_64-unknown-linux-gnu" ? "x86_64-unknown-linux-musl" : target;
  const artifact = manifest.platforms[artifactTarget];
  assert(artifact, `Unsupported runtime tools target: ${target}`);
  const archiveRoot = `ripgrep-${manifest.version}-${artifactTarget}`;
  const cached = join(cacheRoot, artifact.archive);
  await mkdir(cacheRoot, { recursive: true });
  const work = await mkdtemp(join(cacheRoot, "prepare-"));
  try {
    let bytes;
    try { bytes = await readFile(cached); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    if (bytes) {
      assert.equal(bytes.length, artifact.bytes, "Cached ripgrep archive size mismatch");
      assert.equal(sha256(bytes), artifact.sha256, "Cached ripgrep archive checksum mismatch");
    } else {
      const url = `https://github.com/BurntSushi/ripgrep/releases/download/${manifest.version}/${artifact.archive}`;
      const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
      assert(response.ok, `ripgrep download failed: HTTP ${response.status}`);
      bytes = Buffer.from(await response.arrayBuffer());
      assert.equal(bytes.length, artifact.bytes, "ripgrep archive size mismatch");
      assert.equal(sha256(bytes), artifact.sha256, "ripgrep archive checksum mismatch");
      const temporary = join(work, artifact.archive);
      await writeFile(temporary, bytes);
      await rename(temporary, cached);
    }
    if (artifact.archive.endsWith(".zip")) {
      if (process.platform === "win32") {
        const literal = (value) => `'${value.replaceAll("'", "''")}'`;
        await execute("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
          `Expand-Archive -LiteralPath ${literal(cached)} -DestinationPath ${literal(work)}`], { timeout: 30_000 });
      } else {
        await execute("unzip", ["-q", cached, "-d", work], { timeout: 30_000 });
      }
    } else {
      await execute("tar", ["-xzf", cached, "-C", work], { timeout: 30_000 });
    }
    const bin = join(directory, "runtime-tools/bin");
    const licenses = join(directory, "runtime-tools/licenses/ripgrep");
    await mkdir(bin, { recursive: true });
    await mkdir(licenses, { recursive: true });
    // 同一目录不能混用另一个目标的可执行文件。
    await rm(join(bin, target.includes("windows") ? "rg" : "rg.exe"), { force: true });
    const program = target.includes("windows") ? "rg.exe" : "rg";
    await copyFile(join(work, archiveRoot, program), join(bin, program));
    if (!target.includes("windows")) await chmod(join(bin, program), 0o755);
    for (const name of ["LICENSE-MIT", "UNLICENSE", "COPYING"]) {
      await copyFile(join(work, archiveRoot, name), join(licenses, name));
    }
    return join(directory, "runtime-tools");
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [directory, target = nativeTarget()] = process.argv.slice(2);
  assert(directory, "Usage: prepare-runtime-tools.mjs <Host directory> [target triple]");
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
  const cache = resolve(root, process.env.CARGO_TARGET_DIR ?? "target", "runtime-tools-cache");
  console.log(await prepareRuntimeTools(resolve(directory), target, cache));
}
