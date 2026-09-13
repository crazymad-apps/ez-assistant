// Packaging scripts execute in Node, separate from the WebView test environment.
import { execFile } from "node:child_process";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";

const execute = promisify(execFile);
let workspace;
let script;
beforeEach(async () => {
  workspace = await mkdtemp(join(tmpdir(), "ez-sidecar-test-中文 路径-"));
  script = join(workspace, "apps/desktop/scripts/prepare-sidecar.mjs");
  await mkdir(join(workspace, "apps/desktop/scripts"), { recursive: true });
  await copyFile(resolve("scripts/prepare-sidecar.mjs"), script);
});
afterEach(async () => {
  if (resolve(workspace).startsWith(join(tmpdir(), "ez-sidecar-test-"))) await rm(workspace, { recursive: true, force: true });
});

async function fixture(triple, machine) {
  const directory = join(workspace, "target", triple, "release");
  await mkdir(directory, { recursive: true });
  const binary = Buffer.alloc(512);
  binary.write("MZ");
  binary.writeUInt32LE(128, 60);
  binary.writeUInt32LE(0x4550, 128);
  binary.writeUInt16LE(machine, 132);
  await writeFile(join(directory, "ez-assistant-runtime.exe"), binary);
  return binary;
}

describe("Windows sidecar packaging", () => {
  for (const [triple, machine] of [["x86_64-pc-windows-msvc", 0x8664], ["aarch64-pc-windows-msvc", 0xaa64]]) {
    it(`copies the requested ${triple} build`, async () => {
      const expected = await fixture(triple, machine);
      await execute(process.execPath, [script, "--target", triple], { env: { ...process.env, CARGO_TARGET_DIR: join(workspace, "target") } });
      assert.deepEqual(await readFile(join(workspace, "apps/desktop/src-tauri/binaries", `ez-assistant-runtime-${triple}.exe`)), expected);
    });
  }
  it("rejects an x64 Host labeled as ARM64", async () => {
    const triple = "aarch64-pc-windows-msvc";
    await fixture(triple, 0x8664);
    await assert.rejects(execute(process.execPath, [script, "--target", triple], { env: { ...process.env, CARGO_TARGET_DIR: join(workspace, "target") } }), /Host PE architecture does not match/);
  });

  it("does not fall back to the native release when the requested build is missing", async () => {
    await fixture("", 0x8664);
    await assert.rejects(execute(process.execPath, [script, "--target", "aarch64-pc-windows-msvc"], { env: { ...process.env, CARGO_TARGET_DIR: join(workspace, "target") } }), /ENOENT/);
  });
});
