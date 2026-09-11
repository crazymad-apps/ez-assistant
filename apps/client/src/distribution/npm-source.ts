import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { ClientError, object } from "../errors.js";

/** npm 主包显式声明布局；开发入口和 M5 独立归档不猜测 npm 安装位置。 */
export function npmSource(): string | null {
  let metadata: unknown;
  try { metadata = JSON.parse(readFileSync(resolve(import.meta.dirname, "../../package.json"), "utf8")); }
  catch (error) { if (error instanceof Error && "code" in error && error.code === "ENOENT") return null; throw error; }
  if (object(metadata).ezAssistantDistribution !== "npm") return null;
  const name = `@ez-assistant/client-${process.platform}-${process.arch}`;
  const expected = object(object(metadata).optionalDependencies ?? {})[name];
  try {
    const path = createRequire(import.meta.url).resolve(`${name}/package.json`);
    const platform = object(JSON.parse(readFileSync(path, "utf8")) as unknown);
    if (typeof expected !== "string" || platform.name !== name || platform.version !== expected) throw new Error();
    return join(dirname(path), "host/ez-assistant-runtime");
  } catch { throw new ClientError("package_missing", `缺少匹配的平台包 ${name}@${typeof expected === "string" ? expected : "未声明"}，请使用受支持平台并重新安装（不要省略 optionalDependencies）。`); }
}
