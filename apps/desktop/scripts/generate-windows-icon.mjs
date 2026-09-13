import { copyFile, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { run } from "@tauri-apps/cli";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const destination = join(desktop, "src-tauri/icons/windows");
const temporaryRoot = resolve(tmpdir());
const temporary = await mkdtemp(join(temporaryRoot, "ez-assistant-windows-icon-"));
const checkOnly = process.argv.includes("--check");
const files = ["icon.ico", "32x32.png", "128x128.png", "128x128@2x.png"];

try {
  await run(["icon", join(desktop, "app-icon.windows.svg"), "--output", temporary]);
  if (!checkOnly) await mkdir(destination, { recursive: true });
  for (const file of files) {
    const generated = join(temporary, file);
    const target = join(destination, file);
    if (checkOnly) {
      const [expected, actual] = await Promise.all([readFile(generated), readFile(target)]);
      if (!expected.equals(actual)) throw new Error(`Windows icon is stale: ${file}. Run npm run generate:icon:windows.`);
    } else {
      await copyFile(generated, target);
    }
  }
  console.log(checkOnly ? "Windows icons are up to date." : "Generated Windows icons.");
} finally {
  // 只清理本脚本在系统临时目录创建的文件夹，不触碰其他平台图标。
  if (dirname(temporary) !== temporaryRoot) throw new Error("Unexpected temporary icon directory");
  await rm(temporary, { recursive: true, force: true });
}
