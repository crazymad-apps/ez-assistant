import { access, chmod, copyFile, mkdir, mkdtemp, rename, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
const xml = (value) => value.replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[character]);

// pre/post 共用系统脚本目录中的 UID 快照；切换系统用户后拒绝继续交接或改归属。
// 这里只运行新包签名代码，错误原码传回 Installer，绝不启动旧程序或普通 GUI。
export function installationScript(product, finish = false) {
  const target = quote(`/Applications/${product}.app`);
  return `#!/bin/sh
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
if [ "$3" != / ]; then echo 'Installation requires the startup volume' >&2; exit 1; fi
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
${finish ? 'INSTALL_UID=$(cat "$SCRIPT_DIR/install-user")' : 'INSTALL_UID=$(/usr/bin/stat -f %u /dev/console)\nprintf \'%s\\n\' "$INSTALL_UID" > "$SCRIPT_DIR/install-user"\nchmod 600 "$SCRIPT_DIR/install-user"'}
case "$INSTALL_UID" in ''|*[!0-9]*) echo 'Invalid installation user' >&2; exit 1;; esac
if "$SCRIPT_DIR/install-helper" ${finish ? "--finish-installation" : "--prepare-installation"} ${target} "$INSTALL_UID"; then
  echo '${finish ? "Installation finished" : "Installation prepared"}' >&2
else
  status=$?
  echo 'Installation handoff failed; stopped tasks are not restarted automatically' >&2
  echo "Helper exit status: $status" >&2
  exit "$status"
fi
`;
}

export async function createMacosInstaller({ app, output, installerIdentity, signingIdentity, unsignedPkg = false, replace = false }) {
  if (process.platform !== "darwin") throw new Error("macOS packaging requires macOS");
  if (!signingIdentity || !/^Developer ID Application: .+ \([A-Z0-9]{10}\)$/.test(signingIdentity)) throw new Error("Developer ID Application identity is required to sign the standalone installation helper");
  if (!installerIdentity && !unsignedPkg) throw new Error("Developer ID Installer identity is required; local tests must explicitly use --unsigned-pkg");
  if (installerIdentity && !/^Developer ID Installer: .+ \([A-Z0-9]{10}\)$/.test(installerIdentity)) throw new Error("Invalid Developer ID Installer identity");
  app = resolve(app); output = resolve(output);
  if (!app.endsWith(".app") || !output.endsWith(".dmg")) throw new Error("Expected --app .app and --output .dmg");
  if (!replace && await access(output).then(() => true, () => false)) throw new Error("Output already exists");
  const info = join(app, "Contents/Info.plist");
  const field = async (key) => (await run("/usr/bin/plutil", ["-extract", key, "raw", "-o", "-", info], true)).trim();
  const product = await field("CFBundleName");
  const identifier = await field("CFBundleIdentifier");
  const version = await field("CFBundleShortVersionString");
  if (!product || /[/\\\n\r]/.test(product) || basename(app) !== `${product}.app` || !/^[a-zA-Z0-9.-]+$/.test(identifier) || !/^\d+\.\d+\.\d+$/.test(version)) throw new Error("Invalid application identity or version");
  await run("/usr/bin/codesign", ["--verify", "--strict", app]);
  await mkdir(dirname(output), { recursive: true });
  const temporary = await mkdtemp(join(dirname(output), ".ez-macos-installer-"));
  try {
    const payload = join(temporary, "payload");
    const scripts = join(temporary, "scripts");
    const resources = join(temporary, "resources");
    const staging = join(temporary, "dmg");
    for (const path of [payload, scripts, resources, staging]) await mkdir(path);
    await run("/usr/bin/ditto", ["--noextattr", "--noqtn", "--norsrc", app, join(payload, basename(app))]);
    // App 主程序的签名绑定 Info.plist，离开 bundle 后不能直接执行；
    // 将维护副本单独签名为命令行程序，原 App 的签名和资源保持不变。
    const helper = join(scripts, "install-helper");
    await copyFile(join(app, "Contents/MacOS/ez-assistant-desktop"), helper);
    await chmod(helper, 0o755);
    await run("/usr/bin/codesign", ["--force", "--options", "runtime", "--timestamp", "--identifier", `${identifier}.installer-helper`, "--sign", signingIdentity, helper]);
    await run("/usr/bin/codesign", ["--verify", "--strict", helper]);
    for (const [name, finish] of [["preinstall", false], ["postinstall", true]]) {
      await writeFile(join(scripts, name), installationScript(product, finish), { mode: 0o755 });
    }
    const components = join(temporary, "components.plist");
    await run("/usr/bin/pkgbuild", ["--analyze", "--root", payload, components]);
    // 仅升级固定目标，不能通过系统自动重定位覆盖另一份 Dev／独立安装。
    await run("/usr/bin/plutil", ["-replace", "0.BundleIsRelocatable", "-bool", "NO", components]);
    await run("/usr/bin/plutil", ["-replace", "0.BundleOverwriteAction", "-string", "upgrade", components]);
    await run("/usr/bin/pkgbuild", ["--root", payload, "--scripts", scripts, "--component-plist", components, "--identifier", `${identifier}.installer`, "--version", version, "--install-location", "/Applications", "--ownership", "recommended", join(temporary, "component.pkg")]);
    await writeFile(join(resources, "welcome.html"), `<!doctype html><meta charset="utf-8"><h2>安装 ${xml(product)}</h2><p>安装将关闭此应用及其后台服务，并中断正在运行的任务。</p><p>已有配置和会话记录会保留。安装完成后，请正常打开应用。</p><p>开始安装前取消，不影响当前运行的任务。</p>`);
    const distribution = join(temporary, "distribution.xml");
    await writeFile(distribution, `<?xml version="1.0" encoding="utf-8"?><installer-gui-script minSpecVersion="2"><title>${xml(product)}</title><welcome file="welcome.html"/><options customize="never" require-scripts="true" hostArchitectures="arm64"/><domains enable_localSystem="true" enable_currentUserHome="false" enable_anywhere="false"/><choices-outline><line choice="default"/></choices-outline><choice id="default" visible="false"><pkg-ref id="${identifier}.installer"/></choice><pkg-ref id="${identifier}.installer" version="${version}" auth="root">component.pkg</pkg-ref></installer-gui-script>`);
    const pkg = join(staging, `安装 ${product}.pkg`);
    await run("/usr/bin/productbuild", ["--distribution", distribution, "--resources", resources, "--package-path", temporary, ...(installerIdentity ? ["--sign", installerIdentity] : []), pkg]);
    const dmg = join(temporary, "installer.dmg");
    await run("/usr/bin/hdiutil", ["create", "-volname", product, "-srcfolder", staging, "-format", "UDZO", dmg]);
    if (signingIdentity) await run("/usr/bin/codesign", ["--force", "--timestamp", "--sign", signingIdentity, dmg]);
    await rename(dmg, output);
    console.log(`Installer created: ${output}${unsignedPkg ? " (unsigned PKG, local testing only)" : ""}`);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function run(command, args, capture = false) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, { env: { ...process.env, COPYFILE_DISABLE: "1" }, stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit" });
    let text = "";
    child.stdout?.setEncoding("utf8").on("data", (chunk) => { text += chunk; });
    child.on("error", reject);
    child.on("close", (code) => code === 0 ? resolveRun(text) : reject(new Error(`${command} failed with status ${code}`)));
  });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const options = {};
  for (let i = 2; i < process.argv.length; i++) {
    const key = process.argv[i];
    if (key === "--unsigned-pkg") options.unsignedPkg = true;
    else if (["--app", "--output", "--installer-identity", "--signing-identity"].includes(key)) {
      const value = process.argv[++i];
      if (!value || value.startsWith("--")) throw new Error(`Missing ${key}`);
      options[{ "--app": "app", "--output": "output", "--installer-identity": "installerIdentity", "--signing-identity": "signingIdentity" }[key]] = value;
    } else throw new Error(`Unknown packaging argument: ${key}`);
  }
  if (!options.app || !options.output) throw new Error("Usage: package-macos.mjs --app App.app --output installer.dmg --installer-identity ID [--signing-identity ID]; local tests may use --unsigned-pkg");
  await createMacosInstaller(options);
}
