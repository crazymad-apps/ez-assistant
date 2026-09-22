import { cpSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

// 构建和依赖装配全部在 Docker 之外完成；复用既有同源制品装配入口。
if (Number(process.versions.node.split('.')[0]) !== 24) throw new Error('Node 24 is required');
const root = fileURLToPath(new URL('../', import.meta.url));
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const build = spawnSync(npm, ['run', 'build:bundle'], { cwd: root, encoding: 'utf8' });
process.stderr.write(build.stdout ?? '');
process.stderr.write(build.stderr ?? '');
if (build.status !== 0) throw new Error('Center/Admin build failed');
const { directory, version } = JSON.parse(build.stdout.trim().split('\n').at(-1));
const install = spawnSync(npm, ['ci', '--omit=dev', '--ignore-scripts', '--no-audit', '--no-fund'], {
  cwd: directory, stdio: 'inherit',
});
if (install.status !== 0) throw new Error('Production dependency assembly failed');

// 当前生产依赖为跨平台 JS；以后引入原生依赖时必须使用目标平台构建，不能静默复制 macOS 二进制。
const lock = JSON.parse(readFileSync(join(directory, 'package-lock.json'), 'utf8'));
for (const [name, metadata] of Object.entries(lock.packages)) {
  if (metadata.dev) continue;
  if (metadata.os || metadata.cpu || (metadata.hasInstallScript && name !== 'node_modules/@scarf/scarf')) {
    throw new Error(`Review target-platform dependency before packaging: ${name}`);
  }
}
function checkNative(path) {
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isDirectory()) checkNative(child);
    else if (/\.(node|dylib|dll|so)$/.test(entry.name)) throw new Error(`Native dependency requires target build: ${child}`);
  }
}
checkNative(join(directory, 'node_modules'));
cpSync(join(root, 'deploy/Dockerfile'), join(directory, 'Dockerfile'));
writeFileSync(join(directory, '.dockerignore'), '*\n!Dockerfile\n!package.json\n!package-lock.json\n!dist/**\n!public/**\n!node_modules/**\n');
process.stdout.write(JSON.stringify({ directory, version }) + '\n');
