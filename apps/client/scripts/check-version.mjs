// Client 仅核对自己的发布事实；协议与其他产品的构建不依赖本脚本。
import { readFile } from "node:fs/promises";
const read = (path) => readFile(new URL(path, import.meta.url), "utf8");
const [cargo, client, protocol, generated] = await Promise.all([read("../../../Cargo.toml"), read("../package.json"), read("../../../packages/assistant-protocol/package.json"), read("../../../packages/assistant-protocol/src/generated/assistant-protocol.ts")]);
const field = (section, key) => cargo.split(`[${section}]`)[1]?.split(/^\[/m)[0]?.match(new RegExp(`^${key}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
const version = field("workspace.package", "version"), minimum = field("workspace.metadata.ez-assistant", "min_compatible_version");
if (!version || !minimum || JSON.parse(client).version !== version || JSON.parse(protocol).version !== version) throw new Error("Client／Protocol 软件版本与 Cargo 发布源不一致");
for (const [name, value] of [["SOFTWARE_VERSION", version], ["MIN_COMPATIBLE_VERSION", minimum]]) {
  if (!generated.includes(`export const ${name} = ${JSON.stringify(value)} as const;`)) throw new Error("共享协议发布常量已过期，请重新生成");
}
console.log(`Client 发布版本一致：${version}，应用最低兼容版本 ${minimum}`);
