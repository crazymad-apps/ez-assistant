// 只处理已经完成平台签名/公证的最终文件；私钥由 Tauri signer 环境提供，不进入参数或输出。
import { createReadStream } from 'node:fs';
import { copyFile, mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { basename, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const execute = promisify(execFile);
const desktopRoot = fileURLToPath(new URL('../', import.meta.url));
const workspaceRoot = resolve(desktopRoot, '../..');

export async function hashReleaseFile(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

export function requireUpdateSigning() {
  if (!process.env.TAURI_SIGNING_PRIVATE_KEY && !process.env.TAURI_SIGNING_PRIVATE_KEY_PATH)
    throw new Error('Set TAURI_SIGNING_PRIVATE_KEY or TAURI_SIGNING_PRIVATE_KEY_PATH for release signing.');
}

async function sign(path) {
  await execute(process.execPath, [resolve(desktopRoot, 'node_modules/@tauri-apps/cli/tauri.js'), 'signer', 'sign', path],
    { cwd: desktopRoot, timeout: 60000, maxBuffer: 16384 });
  return readFile(`${path}.sig`, 'utf8');
}

/** 可独立在目标运行环境执行；Host 构建信息必须来自实际随包二进制，而不是构建机或管理员输入。 */
export async function createReleaseManifest({ target, host, installer, updater, output }) {
  requireUpdateSigning();
  const targets = { 'windows-x86_64': ['windows', 'x86_64'], 'windows-aarch64': ['windows', 'aarch64'], 'darwin-aarch64': ['macos', 'aarch64'] };
  if (!targets[target]) throw new Error('Unsupported release target');
  const config = JSON.parse(await readFile(resolve(desktopRoot, 'src-tauri/tauri.conf.json'), 'utf8'));
  const workspace = await readFile(resolve(workspaceRoot, 'Cargo.toml'), 'utf8');
  const minimum = workspace.match(/^\[workspace.metadata.ez-assistant\][\s\S]*?^min_compatible_version\s*=\s*"([^"]+)"/m)?.[1];
  const { stdout } = await execute(host, ['--build-info-json'], { timeout: 10000, maxBuffer: 65536 });
  const info = JSON.parse(stdout);
  if (info.version !== config.version || info.min_compatible_version !== minimum
    || info.os !== targets[target][0] || info.arch !== targets[target][1]
    || !Array.isArray(info.enterprise_protocols) || !info.enterprise_protocols.length)
    throw new Error('Bundled Host metadata does not match release version, compatibility or target.');
  await mkdir(output, { recursive: true });
  const sources = installer === updater ? [{ path: installer, purposes: ['installer', 'updater'] }]
    : [{ path: installer, purposes: ['installer'] }, { path: updater, purposes: ['updater'] }];
  const files = [];
  for (const source of sources) {
    const name = basename(source.path).replaceAll(' ', '-'), destination = resolve(output, name);
    if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,180}$/.test(name) || files.some(file => file.name === name))
      throw new Error('Release artifact names must be distinct portable filenames.');
    if (resolve(source.path) !== destination) await copyFile(source.path, destination);
    const before = await stat(destination);
    if (!before.isFile() || before.size < 1 || before.size > 536870912) throw new Error('Release artifact must be between 1 byte and 512 MiB.');
    const sha256 = await hashReleaseFile(destination);
    const signature = source.purposes.includes('updater') ? await sign(destination) : undefined;
    const after = await stat(destination);
    // 签名后的字节变化拒绝发布；正常路径不重复扫描大文件。
    if (before.size !== after.size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) throw new Error('Artifact changed during signing.');
    files.push({ name, purposes: source.purposes, size: before.size, sha256, ...(signature ? { signature } : {}) });
  }
  const manifest = { product: 'ez-assistant', version: config.version, min_supported_version: minimum, target,
    host: { version: info.version, min_supported_version: info.min_compatible_version, enterprise_protocols: info.enterprise_protocols }, files };
  const path = resolve(output, 'release.json');
  await writeFile(path, JSON.stringify(manifest, null, 2) + '\n');
  await sign(path);
  return manifest;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [target, host, installer, updater, output] = process.argv.slice(2);
  if (!output) throw new Error('Usage: release-manifest.mjs <target> <host> <installer> <updater> <output>');
  await createReleaseManifest({ target, host: resolve(host), installer: resolve(installer), updater: resolve(updater), output: resolve(output) });
  console.log('Signed release files prepared.');
}
