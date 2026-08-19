import { lstat, mkdir, mkdtemp, readdir, realpath, rm, stat } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { platform, tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const arguments_ = parseArguments(process.argv.slice(2));

if (arguments_.help) {
  printUsage();
  process.exit(0);
}
if (platform() !== "darwin") {
  throw new Error("macOS release verification must run on macOS");
}

const appPath = await requirePath(arguments_.app, "--app", ".app", true);
const dmgPath = await requirePath(arguments_.dmg, "--dmg", ".dmg", false);
const teamId = requireValue(arguments_.teamId, "--team-id");
if (!/^[A-Z0-9]{10}$/.test(teamId)) {
  throw new Error("--team-id must be a 10-character Apple Team ID");
}
if (Boolean(arguments_.submissionId) !== Boolean(arguments_.keychainProfile)) {
  throw new Error("--submission-id and --keychain-profile must be provided together");
}

console.log(`Verifying release artifacts for Team ${teamId}.`);
await verifyApp(appPath, teamId);
verifyDmg(dmgPath, teamId);
await verifyMountedDmg(dmgPath, teamId);

if (arguments_.submissionId && arguments_.keychainProfile) {
  verifyNotarization(
    arguments_.submissionId,
    arguments_.keychainProfile,
  );
}

console.log("macOS release verification passed.");

async function verifyApp(targetPath, expectedTeamId) {
  run("codesign", ["--verify", "--strict", "--verbose=4", targetPath]);
  const appSignature = inspectSignature(targetPath, expectedTeamId, true);
  assertDeveloperId(appSignature, targetPath, expectedTeamId);
  assertSafeEntitlements(targetPath);

  const machOFiles = await findMachOFiles(resolve(targetPath, "Contents"));
  if (machOFiles.length === 0) {
    throw new Error(`No Mach-O code found in ${targetPath}`);
  }
  for (const machOPath of machOFiles) {
    run("codesign", ["--verify", "--strict", "--verbose=4", machOPath]);
    const fileDescription = run("file", ["-b", machOPath], false).stdout.trim();
    const requiresRuntime = fileDescription.includes("executable");
    const signature = inspectSignature(machOPath, expectedTeamId, requiresRuntime);
    assertDeveloperId(signature, machOPath, expectedTeamId);
    const architectures = run("lipo", ["-archs", machOPath], false).stdout.trim();
    if (architectures !== "arm64") {
      throw new Error(`${machOPath} must contain only arm64 code; found ${architectures}`);
    }
  }

  run("spctl", ["--assess", "--type", "execute", "--verbose=4", targetPath]);
  run("xcrun", ["stapler", "validate", targetPath]);
}

function verifyDmg(targetPath, expectedTeamId) {
  run("hdiutil", ["verify", targetPath]);
  run("codesign", ["--verify", "--strict", "--verbose=4", targetPath]);
  const signature = inspectSignature(targetPath, expectedTeamId, false);
  assertDeveloperId(signature, targetPath, expectedTeamId);
  run("xcrun", ["stapler", "validate", targetPath]);
  run("spctl", [
    "--assess",
    "--type",
    "open",
    "--context",
    "context:primary-signature",
    "--verbose=4",
    targetPath,
  ]);
}

async function verifyMountedDmg(targetPath, expectedTeamId) {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "ez-assistant-release-"));
  const mountPoint = join(temporaryRoot, "volume");
  await mkdir(mountPoint);
  let attached = false;
  let verificationError;
  try {
    run("hdiutil", [
      "attach",
      "-readonly",
      "-nobrowse",
      "-mountpoint",
      mountPoint,
      targetPath,
    ]);
    attached = true;
    const entries = await readdir(mountPoint);
    const applications = entries.filter((entry) => entry.endsWith(".app"));
    if (applications.length !== 1) {
      throw new Error(`DMG must contain exactly one top-level app; found ${applications.length}`);
    }
    const applicationsLink = join(mountPoint, "Applications");
    const applicationsMetadata = await lstat(applicationsLink).catch(() => undefined);
    if (!applicationsMetadata?.isSymbolicLink()) {
      throw new Error("DMG must contain a top-level Applications symlink");
    }
    await verifyApp(join(mountPoint, applications[0]), expectedTeamId);
  } catch (error) {
    verificationError = error;
  }

  let detachError;
  if (attached) {
    const detach = run("hdiutil", ["detach", mountPoint], false, true);
    if (detach.status !== 0) {
      detachError = new Error(`Unable to detach verification volume ${mountPoint}`);
    }
  }
  if (!detachError) {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
  if (verificationError) {
    throw verificationError;
  }
  if (detachError) {
    throw detachError;
  }
}

function verifyNotarization(submissionId, keychainProfile) {
  const info = run("xcrun", [
    "notarytool",
    "info",
    submissionId,
    "--keychain-profile",
    keychainProfile,
    "--output-format",
    "json",
  ], false).stdout;
  const parsed = JSON.parse(info);
  if (parsed.status !== "Accepted") {
    throw new Error(`Notarization ${submissionId} has status ${parsed.status ?? "unknown"}`);
  }
  run("xcrun", [
    "notarytool",
    "log",
    submissionId,
    "--keychain-profile",
    keychainProfile,
  ]);
}

function inspectSignature(targetPath, expectedTeamId, requiresRuntime) {
  const output = run("codesign", ["-d", "--verbose=4", targetPath], false).stderr;
  const authority = output
    .split("\n")
    .find((line) => line.startsWith("Authority="))
    ?.slice("Authority=".length);
  const teamIdentifier = output
    .split("\n")
    .find((line) => line.startsWith("TeamIdentifier="))
    ?.slice("TeamIdentifier=".length);
  const flags = output
    .split("\n")
    .find((line) => line.startsWith("CodeDirectory"));
  if (requiresRuntime && !flags?.includes("runtime")) {
    throw new Error(`${targetPath} is not signed with Hardened Runtime`);
  }
  return { authority, teamIdentifier, expectedTeamId };
}

function assertDeveloperId(signature, targetPath, expectedTeamId) {
  if (!signature.authority?.startsWith("Developer ID Application:")) {
    throw new Error(`${targetPath} is not signed by a Developer ID Application certificate`);
  }
  if (signature.teamIdentifier !== expectedTeamId) {
    throw new Error(
      `${targetPath} TeamIdentifier ${signature.teamIdentifier ?? "missing"} does not match ${expectedTeamId}`,
    );
  }
}

function assertSafeEntitlements(targetPath) {
  const result = run("codesign", ["-d", "--entitlements", "-", targetPath], false);
  const output = `${result.stdout}${result.stderr}`;
  const forbidden = [
    "com.apple.security.get-task-allow",
    "com.apple.security.cs.disable-library-validation",
    "com.apple.security.cs.allow-unsigned-executable-memory",
    "com.apple.security.cs.allow-jit",
  ];
  const present = forbidden.filter((entitlement) => output.includes(entitlement));
  if (present.length > 0) {
    throw new Error(`${targetPath} contains forbidden entitlements: ${present.join(", ")}`);
  }
}

async function findMachOFiles(root) {
  const found = [];
  const pending = [root];
  while (pending.length > 0) {
    const current = pending.pop();
    const entries = await readdir(current, { withFileTypes: true });
    for (const entry of entries) {
      const entryPath = join(current, entry.name);
      if (entry.isDirectory()) {
        pending.push(entryPath);
      } else if (entry.isFile()) {
        const description = run("file", ["-b", entryPath], false).stdout;
        if (description.includes("Mach-O")) {
          found.push(entryPath);
        }
      }
    }
  }
  return found.sort();
}

async function requirePath(value, option, extension, expectDirectory) {
  const supplied = requireValue(value, option);
  const targetPath = await realpath(resolve(supplied));
  const metadata = await stat(targetPath);
  if (expectDirectory ? !metadata.isDirectory() : !metadata.isFile()) {
    throw new Error(`${option} has the wrong file type: ${targetPath}`);
  }
  if (!targetPath.endsWith(extension)) {
    throw new Error(`${option} must reference a ${extension} artifact`);
  }
  return targetPath;
}

function requireValue(value, option) {
  if (!value) {
    throw new Error(`${option} is required`);
  }
  return value;
}

function parseArguments(values) {
  const parsed = {};
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === "--help") {
      parsed.help = true;
      continue;
    }
    const next = values[index + 1];
    if (!next || next.startsWith("--")) {
      throw new Error(`${value} requires a value`);
    }
    const key = {
      "--app": "app",
      "--dmg": "dmg",
      "--team-id": "teamId",
      "--submission-id": "submissionId",
      "--keychain-profile": "keychainProfile",
    }[value];
    if (!key) {
      throw new Error(`Unknown option ${value}`);
    }
    parsed[key] = next;
    index += 1;
  }
  return parsed;
}

function run(command, arguments_, printOutput = true, allowFailure = false) {
  const result = spawnSync(command, arguments_, {
    cwd: scriptDirectory,
    encoding: "utf8",
    shell: false,
    stdio: ["ignore", "pipe", "pipe"],
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) {
    throw result.error;
  }
  if (printOutput) {
    if (result.stdout) {
      process.stdout.write(result.stdout);
    }
    if (result.stderr) {
      process.stderr.write(result.stderr);
    }
  }
  if (!allowFailure && result.status !== 0) {
    const detail = `${result.stdout ?? ""}${result.stderr ?? ""}`.trim();
    throw new Error(`${command} exited with ${result.status ?? "unknown"}${detail ? `: ${detail}` : ""}`);
  }
  return result;
}

function printUsage() {
  console.log(`Usage:
  node scripts/verify-macos-release.mjs \\
    --app /absolute/path/to/EZ\\ Assistant.app \\
    --dmg /absolute/path/to/EZ\\ Assistant.dmg \\
    --team-id TEAMID

Optional notarization-log verification:
    --submission-id UUID --keychain-profile PROFILE`);
}
