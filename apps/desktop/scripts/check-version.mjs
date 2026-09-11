// 仅核对 Desktop 构建实际消费的发布事实；不读取 Client 工程或依赖其安装。
import { readFile } from "node:fs/promises";

const read = (path) => readFile(new URL(path, import.meta.url), "utf8");
const [cargo, packageText, tauriText, generated, protocolPackageText] = await Promise.all([
  read("../../../Cargo.toml"), read("../package.json"),
  read("../src-tauri/tauri.conf.json"), read("../../../packages/assistant-protocol/src/generated/assistant-protocol.ts"),
  read("../../../packages/assistant-protocol/package.json"),
]);
function field(section, key) {
  const body = cargo.split(`[${section}]`)[1]?.split(/^\[/m)[0];
  return body?.match(new RegExp(`^${key}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
}
const version = field("workspace.package", "version");
const minimum = field("workspace.metadata.ez-assistant", "min_compatible_version");
function parse(value) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(value ?? "")) throw new Error("发布版本不是规范的软件版本号");
  const parts = value.split(".").map(Number);
  if (parts.some((part) => part > 0xffffffff)) throw new Error("软件版本超出 u32 范围");
  return parts;
}
const actual = parse(version);
const lower = parse(minimum);
const difference = actual.map((value, index) => value - lower[index]).find((value) => value !== 0) ?? 0;
if (difference < 0) throw new Error("最低兼容版本不能高于当前软件版本");
for (const [name, value] of [["Desktop", JSON.parse(packageText).version], ["Tauri", JSON.parse(tauriText).version], ["Protocol", JSON.parse(protocolPackageText).version]]) {
  if (value !== version) throw new Error(`${name} 软件版本与 Cargo 发布源不一致`);
}
for (const [name, value] of [["SOFTWARE_VERSION", version], ["MIN_COMPATIBLE_VERSION", minimum]]) {
  if (!generated.includes(`export const ${name} = ${JSON.stringify(value)} as const;`)) {
    throw new Error(`${name} 已过期，请重新生成应用协议`);
  }
}
console.log(`Desktop 发布版本一致：${version}，应用最低兼容版本 ${minimum}`);
