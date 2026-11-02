import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [, , targetName, binaryPath] = process.argv;

if (!targetName || !binaryPath) {
  console.error("Usage: node scripts/package-platform.mjs <darwin-arm64|darwin-x64|linux-x64|linux-arm64|win32-x64> <binary>");
  process.exit(1);
}

const meta = {
  "darwin-arm64": { os: ["darwin"], cpu: ["arm64"], ext: "" },
  "darwin-x64": { os: ["darwin"], cpu: ["x64"], ext: "" },
  "linux-x64": { os: ["linux"], cpu: ["x64"], ext: "" },
  "linux-arm64": { os: ["linux"], cpu: ["arm64"], ext: "" },
  "win32-x64": { os: ["win32"], cpu: ["x64"], ext: ".exe" }
}[targetName];

if (!meta) {
  console.error(`Unsupported target package: ${targetName}`);
  process.exit(1);
}

if (!fs.existsSync(binaryPath)) {
  console.error(`Binary not found: ${binaryPath}`);
  process.exit(1);
}

const rootPackage = JSON.parse(fs.readFileSync(path.resolve("package.json"), "utf8"));
const outDir = path.resolve("npm", "platform", targetName);
const binDir = path.join(outDir, "bin");
fs.rmSync(outDir, { recursive: true, force: true });
fs.mkdirSync(binDir, { recursive: true });
const outBinary = path.join(binDir, `agent-browser-cli${meta.ext}`);
fs.copyFileSync(binaryPath, outBinary);

// 平台包发布的是运行二进制，默认移除符号表以减小 npm 包体积；本地 target/release 保持不变，便于排查。
if (process.env.AGENT_BROWSER_CLI_SKIP_STRIP !== "1") {
  const strip = spawnSync("strip", [outBinary], { stdio: "ignore" });
  if (strip.status !== 0) {
    console.warn(`[package-platform] strip skipped or failed for ${targetName}`);
  }
}

fs.writeFileSync(
  path.join(outDir, "package.json"),
  JSON.stringify(
    {
      name: `${rootPackage.name}-${targetName}`,
      version: rootPackage.version,
      description: `${rootPackage.description} (${targetName} native binary).`,
      license: rootPackage.license,
      repository: rootPackage.repository,
      publishConfig: { access: "public", provenance: true },
      os: meta.os,
      cpu: meta.cpu,
      files: ["bin"]
    },
    null,
    2
  ) + "\n",
  "utf8"
);
