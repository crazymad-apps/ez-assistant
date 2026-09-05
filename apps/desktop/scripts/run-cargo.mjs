import { spawn } from "node:child_process";

import { selectMacDeveloperDirectory } from "./mac-developer-directory.mjs";

selectMacDeveloperDirectory("cargo");

const child = spawn("cargo", process.argv.slice(2), {
  env: process.env,
  stdio: "inherit",
  shell: false,
});
child.once("error", (error) => {
  console.error(error);
  process.exitCode = 1;
});
child.once("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code ?? 1;
});
