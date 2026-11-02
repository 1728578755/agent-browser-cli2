#!/usr/bin/env node

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

/**
 * 当前平台对应的预编译子包名；没有预编译包时返回 null。
 *
 * 平台清单与 package.json 的 optionalDependencies、publish.yml 的 matrix 必须一致。
 * win32-arm64（Windows on ARM）没有预编译包——发一个从未在 CI 构建过的包比不发布更糟，
 * 所以这里明确返回 null，由 resolveBinary 给出"自编译"的可操作提示，而不是抛
 * Unsupported platform 让用户无处可去。
 */
function platformPackageName(platform = process.platform, arch = process.arch) {
  if (platform === "darwin" && arch === "arm64") return "agent-browser-cli2-darwin-arm64";
  if (platform === "darwin" && arch === "x64") return "agent-browser-cli2-darwin-x64";
  if (platform === "linux" && arch === "x64") return "agent-browser-cli2-linux-x64";
  if (platform === "linux" && arch === "arm64") return "agent-browser-cli2-linux-arm64";
  if (platform === "win32" && arch === "x64") return "agent-browser-cli2-win32-x64";
  return null;
}

function resolveBinary() {
  if (process.env.AGENT_BROWSER_CLI_BIN) return process.env.AGENT_BROWSER_CLI_BIN;
  const ext = process.platform === "win32" ? ".exe" : "";
  const pkgName = platformPackageName();
  if (pkgName) {
    try {
      return require.resolve(`${pkgName}/bin/agent-browser-cli${ext}`);
    } catch {
      // 子包没装（--omit=optional 等），落到下面的本地/自编译路径给出可操作提示
    }
  }
  // 本地开发布局：<repo>/npm/bin → ../../target/release（cargo build --release 的产物）
  // npm 安装布局：  <prefix>/node_modules/agent-browser-cli2/npm/bin
  //                 → <pkg>/target/release —— 发布包的 files 里没有 target，
  //                 所以全局安装时这里必然不存在，只是为了本地开发不依赖 resolve。
  const local = path.resolve(__dirname, "..", "..", "target", "release", `agent-browser-cli${ext}`);
  if (fs.existsSync(local)) return local;
  // 没有预编译包的平台：不能只喊 Unsupported，必须给出可执行的自救路径。
  if (!pkgName) {
    throw new Error(
      `agent-browser-cli 未提供 ${process.platform}-${process.arch} 的预编译包。` +
        `目前已发布：macOS (arm64/x64)、Linux (x64/arm64)、Windows (x64)。\n` +
        `可选做法：\n` +
        `  1. 自行编译：在仓库根目录执行 "cargo build --release"，产物为 target/release/agent-browser-cli${ext}；\n` +
        `  2. 指定二进制：设置环境变量 AGENT_BROWSER_CLI_BIN 指向该产物（建议写进 shell 配置，避免新终端失效）；\n` +
        `  3. Windows on ARM 可改装 x64 版 Node.js（系统会做 x64 仿真），此时 process.arch 为 x64，可直接使用预编译包。`,
    );
  }
  throw new Error(
    `agent-browser-cli 原生二进制缺失（可选依赖 ${pkgName}，回退路径 ${local}）。` +
      `请勿使用 --ignore-optional / --no-optional 安装，可执行 ` +
      `"npm install agent-browser-cli2@latest --force" 重装；` +
      `本地开发请先跑 "cargo build --release"。`,
  );
}

/**
 * 推导包根目录（含 skills/ 的那一层）。
 *
 * 发布包与本地仓库是同一种布局，只需要向上两级：
 *   <pkg>/npm/bin/agent-browser-cli.js  ← __dirname
 *   <pkg>/skills/agent-browser-cli/SKILL.md
 * 历史实现只退了一级，得到 <pkg>/npm，导致 npm 安装下 install-skill 必然失败。
 * 兜底：极少数历史布局把 skills 放在 npm/skills 下时仍能命中。
 */
function resolvePackageDir() {
  const packageDir = path.resolve(__dirname, "..", "..");
  if (fs.existsSync(path.join(packageDir, "skills", "agent-browser-cli", "SKILL.md"))) {
    return packageDir;
  }
  const legacyDir = path.resolve(__dirname, "..");
  if (fs.existsSync(path.join(legacyDir, "skills", "agent-browser-cli", "SKILL.md"))) {
    return legacyDir;
  }
  return packageDir;
}

const bin = resolveBinary();
const env = { ...process.env, AGENT_BROWSER_CLI_PACKAGE_DIR: resolvePackageDir() };
const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit", env });
if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}
// 被信号杀死时 status === null，直接 ?? 0 会让 CI 误判成功。
if (result.signal) {
  console.error(`agent-browser-cli terminated by ${result.signal}`);
  process.exit(1);
}
process.exit(result.status ?? 0);
