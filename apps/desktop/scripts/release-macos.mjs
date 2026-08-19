import { access, readFile, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { createReadStream } from "node:fs";
import { createHash } from "node:crypto";
import { basename, dirname, resolve } from "node:path";
import { arch, env, platform } from "node:process";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const desktopRoot = resolve(scriptDirectory, "..");
const workspaceRoot = resolve(desktopRoot, "../..");
const signingIdentity = requiredEnvironment("APPLE_SIGNING_IDENTITY");
const teamId = requiredEnvironment("APPLE_TEAM_ID");
const apiIssuer = requiredEnvironment("APPLE_API_ISSUER");
const apiKey = requiredEnvironment("APPLE_API_KEY");
const apiKeyPath = resolve(requiredEnvironment("APPLE_API_KEY_PATH"));

if (platform !== "darwin" || arch !== "arm64") {
  throw new Error("macOS release builds must run on an Apple Silicon Mac");
}
if (!/^Developer ID Application: .+ \([A-Z0-9]{10}\)$/.test(signingIdentity)) {
  throw new Error("APPLE_SIGNING_IDENTITY must name a Developer ID Application identity");
}
if (!/^[A-Z0-9]{10}$/.test(teamId)) {
  throw new Error("APPLE_TEAM_ID must be a 10-character Apple Team ID");
}
if (!signingIdentity.endsWith(`(${teamId})`)) {
  throw new Error("APPLE_SIGNING_IDENTITY and APPLE_TEAM_ID refer to different teams");
}
if (!/^[0-9a-fA-F-]{36}$/.test(apiIssuer)) {
  throw new Error("APPLE_API_ISSUER must be the UUID shown for the App Store Connect Team Key");
}
if (!/^[A-Z0-9]{10}$/.test(apiKey)) {
  throw new Error("APPLE_API_KEY must be the 10-character App Store Connect Key ID");
}
if (basename(apiKeyPath) !== `AuthKey_${apiKey}.p8`) {
  throw new Error("APPLE_API_KEY_PATH filename does not match APPLE_API_KEY");
}

await access(apiKeyPath, constants.R_OK);
const apiKeyMetadata = await stat(apiKeyPath);
if (!apiKeyMetadata.isFile()) {
  throw new Error("APPLE_API_KEY_PATH must reference a regular .p8 file");
}
if ((apiKeyMetadata.mode & 0o077) !== 0) {
  throw new Error("APPLE_API_KEY_PATH must not be accessible by group or other users");
}

const identities = await capture("security", ["find-identity", "-v", "-p", "codesigning"]);
if (!identities.includes(`"${signingIdentity}"`)) {
  throw new Error("APPLE_SIGNING_IDENTITY is not a valid identity in the current Keychain");
}

const tauriConfig = JSON.parse(
  await readFile(resolve(desktopRoot, "src-tauri/tauri.conf.json"), "utf8"),
);
const packageConfig = JSON.parse(
  await readFile(resolve(desktopRoot, "package.json"), "utf8"),
);
const workspaceManifest = await readFile(resolve(workspaceRoot, "Cargo.toml"), "utf8");
const productName = tauriConfig.productName;
const version = tauriConfig.version;
if (typeof productName !== "string" || typeof version !== "string") {
  throw new Error("tauri.conf.json must define productName and version");
}
const workspaceVersion = workspaceManifest.match(
  /^\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
)?.[1];
if (packageConfig.version !== version || workspaceVersion !== version) {
  throw new Error(
    `release versions must match (tauri=${version}, npm=${packageConfig.version ?? "missing"}, rust=${workspaceVersion ?? "missing"})`,
  );
}

console.log(`Signing identity validated for Team ${teamId}.`);
console.log("App Store Connect API credentials and private-key permissions validated.");
console.log(`Release version ${version} validated across Tauri, npm, and Rust workspace.`);
await run("npm", [
  "run",
  "tauri",
  "--",
  "build",
  "--target",
  "aarch64-apple-darwin",
  "--bundles",
  "app,dmg",
], desktopRoot);

const bundleRoot = resolve(
  workspaceRoot,
  "target/aarch64-apple-darwin/release/bundle",
);
const appPath = resolve(bundleRoot, "macos", `${productName}.app`);
const dmgPath = resolve(bundleRoot, "dmg", `${productName}_${version}_aarch64.dmg`);

const dmgSubmissionId = await notarizeDmg(dmgPath);
console.log(`DMG notarization accepted: ${dmgSubmissionId}`);
await run("xcrun", ["stapler", "staple", dmgPath], desktopRoot);

await run(
  process.execPath,
  [
    resolve(scriptDirectory, "verify-macos-release.mjs"),
    "--app",
    appPath,
    "--dmg",
    dmgPath,
    "--team-id",
    teamId,
  ],
  desktopRoot,
);

const dmgMetadata = await stat(dmgPath);
const dmgSha256 = await hashFile(dmgPath);
console.log("macOS release completed.");
console.log(`App: ${appPath}`);
console.log(`DMG: ${dmgPath}`);
console.log(`DMG size: ${dmgMetadata.size} bytes`);
console.log(`DMG SHA-256: ${dmgSha256}`);
console.log(`DMG notarization submission: ${dmgSubmissionId}`);

async function notarizeDmg(targetPath) {
  console.log("Submitting the signed DMG for Apple notarization.");
  const credentials = [
    "--key-id",
    apiKey,
    "--key",
    apiKeyPath,
    "--issuer",
    apiIssuer,
  ];
  const submission = await capture("xcrun", [
    "notarytool",
    "submit",
    targetPath,
    "--wait",
    "--output-format",
    "json",
    ...credentials,
  ]);
  const parsed = parseJsonOutput(submission, "notarytool submit");
  if (parsed.status !== "Accepted" || typeof parsed.id !== "string") {
    throw new Error(
      `DMG notarization was not accepted (status: ${parsed.status ?? "unknown"})`,
    );
  }
  await run(
    "xcrun",
    ["notarytool", "log", parsed.id, ...credentials],
    desktopRoot,
  );
  return parsed.id;
}

function parseJsonOutput(output, operation) {
  try {
    return JSON.parse(output);
  } catch {
    throw new Error(`${operation} returned an invalid JSON response`);
  }
}

function hashFile(path) {
  return new Promise((resolvePromise, rejectPromise) => {
    const hash = createHash("sha256");
    const input = createReadStream(path);
    input.once("error", rejectPromise);
    input.on("data", (chunk) => hash.update(chunk));
    input.once("end", () => resolvePromise(hash.digest("hex")));
  });
}

function requiredEnvironment(name) {
  const value = env[name]?.trim();
  if (!value) {
    throw new Error(`${name} is required for a signed and notarized macOS release`);
  }
  return value;
}

function capture(command, arguments_) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, arguments_, {
      cwd: desktopRoot,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.once("error", rejectPromise);
    child.once("exit", (code) => {
      if (code === 0) {
        resolvePromise(`${stdout}${stderr}`);
      } else {
        rejectPromise(new Error(`${command} exited with ${code ?? "unknown"}`));
      }
    });
  });
}

function run(command, arguments_, workingDirectory) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, arguments_, {
      cwd: workingDirectory,
      env,
      shell: false,
      stdio: "inherit",
    });
    child.once("error", rejectPromise);
    child.once("exit", (code) => {
      if (code === 0) {
        resolvePromise();
      } else {
        rejectPromise(new Error(`${command} exited with ${code ?? "unknown"}`));
      }
    });
  });
}
