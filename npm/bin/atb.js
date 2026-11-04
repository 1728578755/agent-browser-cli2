#!/usr/bin/env node

const { spawn } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

/** 平台子包内的二进制文件名。 */
const BINARY_NAMES = ["atb"];

/**
 * 当前平台对应的预编译子包名；没有预编译包时返回 null。
 *
 * 平台清单与 package.json 的 optionalDependencies、publish.yml 的 matrix 必须一致。
 * win32-arm64（Windows on ARM）没有预编译包——发一个从未在 CI 构建过的包比不发布更糟，
 * 所以这里明确返回 null，由 resolveBinary 给出"自编译"的可操作提示，而不是抛
 * Unsupported platform 让用户无处可去。Alpine(musl) 同理：发布了 glibc 二进制也跑不起来。
 */
function platformPackageName(platform = process.platform, arch = process.arch) {
  if (platform === "darwin" && arch === "arm64") return "@g1728578755/atb-darwin-arm64";
  if (platform === "darwin" && arch === "x64") return "@g1728578755/atb-darwin-x64";
  if (platform === "linux" && arch === "x64" && !isMusl()) return "@g1728578755/atb-linux-x64";
  if (platform === "linux" && arch === "arm64" && !isMusl()) return "@g1728578755/atb-linux-arm64";
  if (platform === "win32" && arch === "x64") return "@g1728578755/atb-win32-x64";
  return null;
}

/**
 * 是否为 musl libc（Alpine 等）。
 *
 * Node 官方 Linux 构建在 report 里带 glibcVersionRuntime；没有这一项基本就是 musl。
 * 以前把 Alpine 当 linux-x64，装上 glibc 二进制后 spawn 直接 ENOENT，报错还引导用户
 * 「重装 --force」，越修越远。
 */
function isMusl() {
  if (process.platform !== "linux") return false;
  try {
    const report = process.report?.getReport();
    return !report?.header?.glibcVersionRuntime;
  } catch {
    return false;
  }
}

function resolveBinary() {
  if (process.env.ATB_BIN) return process.env.ATB_BIN;
  if (process.env.AGENT_BROWSER_CLI_BIN) return process.env.AGENT_BROWSER_CLI_BIN;
  const ext = process.platform === "win32" ? ".exe" : "";
  const pkgName = platformPackageName();
  if (pkgName) {
    for (const name of BINARY_NAMES) {
      try {
        return require.resolve(`${pkgName}/bin/${name}${ext}`);
      } catch {
        // 继续试下一个名字 / 走本地开发路径
      }
    }
  }
  // 本地开发布局：<repo>/npm/bin → ../../target/release（cargo build --release 的产物）
  // npm 安装布局：  <prefix>/node_modules/@g1728578755/atb/npm/bin
  //                 → <pkg>/target/release —— 发布包的 files 里没有 target，
  //                 所以全局安装时这里必然不存在，只是为了本地开发不依赖 resolve。
  const tried = [];
  for (const name of BINARY_NAMES) {
    const candidate = path.resolve(__dirname, "..", "..", "target", "release", `${name}${ext}`);
    tried.push(candidate);
    if (fs.existsSync(candidate)) return candidate;
  }
  // 没有预编译包的平台：不能只喊 Unsupported，必须给出可执行的自救路径。
  if (!pkgName) {
    const muslHint = isMusl()
      ? `\n  4. Alpine/musl 环境：官方预编译包依赖 glibc，需自行编译（cargo build --release）或改用 glibc 基础镜像（如 node:20-bookworm-slim）。`
      : "";
    throw new Error(
      `atb 未提供 ${process.platform}-${process.arch} 的预编译包。` +
        `目前已发布：macOS (arm64/x64)、Linux glibc (x64/arm64)、Windows (x64)。\n` +
        `可选做法：\n` +
        `  1. 自行编译：在仓库根目录执行 "cargo build --release"，产物为 target/release/atb${ext}；\n` +
        `  2. 指定二进制：设置环境变量 ATB_BIN 指向该产物（建议写进 shell 配置，避免新终端失效）；\n` +
        `  3. Windows on ARM 可改装 x64 版 Node.js（系统会做 x64 仿真），此时 process.arch 为 x64，可直接使用预编译包。` +
        muslHint,
    );
  }
  throw new Error(
    `atb 原生二进制缺失（可选依赖 ${pkgName}）。已尝试：${tried.join("、")}。` +
      `请勿使用 --ignore-optional / --no-optional 安装，可执行 ` +
      `"npm install @g1728578755/atb@latest --force" 重装；` +
      `本地开发请先跑 "cargo build --release"。`,
  );
}

/**
 * 推导包根目录（含 skills/ 的那一层）。
 *
 * 发布包与本地仓库是同一种布局，只需要向上两级：
 *   <pkg>/npm/bin/atb.js  ← __dirname
 *   <pkg>/skills/atb/SKILL.md
 * 历史实现只退了一级，得到 <pkg>/npm，导致 npm 安装下 install-skill 必然失败。
 * 兜底：极少数历史布局把 skills 放在 npm/skills 下时仍能命中。
 */
function resolvePackageDir() {
  const packageDir = path.resolve(__dirname, "..", "..");
  if (fs.existsSync(path.join(packageDir, "skills", "atb", "SKILL.md"))) {
    return packageDir;
  }
  const legacyDir = path.resolve(__dirname, "..");
  if (fs.existsSync(path.join(legacyDir, "skills", "atb", "SKILL.md"))) {
    return legacyDir;
  }
  return packageDir;
}

const SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];
const SIGNAL_EXIT_BASE = 128;

const bin = resolveBinary();
const env = {
  ...process.env,
  // 新前缀优先；旧前缀保留，避免已写进脚本的 AGENT_BROWSER_CLI_PACKAGE_DIR 失效。
  ATB_PACKAGE_DIR: resolvePackageDir(),
  AGENT_BROWSER_CLI_PACKAGE_DIR: resolvePackageDir(),
};

// 用 spawn（而不是 spawnSync）才能真正转发信号：Ctrl-C 时要让 atb 自己收尾，
// 否则 daemon 里的在途请求会一直挂到超时。
const child = spawn(bin, process.argv.slice(2), { stdio: "inherit", env });

for (const signal of SIGNALS) {
  process.on(signal, () => {
    if (!child.killed) {
      try {
        child.kill(signal);
      } catch {
        // 子进程已退出：忽略
      }
    }
  });
}

child.on("error", (error) => {
  console.error(error.message);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    // POSIX 约定：被信号终止时退出码为 128+n（SIGINT=2 → 130），
    // 以前统一 exit(1)，脚本无法区分「被中断」与「执行失败」。
    const number = require("node:os").constants.signals[signal];
    console.error(`atb terminated by ${signal}`);
    process.exit(typeof number === "number" ? SIGNAL_EXIT_BASE + number : 1);
  }
  process.exit(code ?? 0);
});
