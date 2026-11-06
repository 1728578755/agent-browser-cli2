#!/usr/bin/env node

const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

/** 平台子包内的二进制文件名。 */
const BINARY_NAMES = ["atb"];

/** 包根目录：<pkg>/npm/bin/atb.js → 上两级。 */
const PACKAGE_DIR = path.resolve(__dirname, "..", "..");

/** 平台清单唯一真相源。发布包会带上 scripts/platforms.json；缺失时退回内置表。 */
function loadPlatforms() {
  const configPath = path.join(PACKAGE_DIR, "scripts", "platforms.json");
  try {
    const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
    if (Array.isArray(config.platforms) && config.platforms.length > 0) return config.platforms;
  } catch {
    // 忽略：走内置兜底
  }
  return [
    { name: "darwin-arm64", os: "darwin", cpu: "arm64" },
    { name: "darwin-x64", os: "darwin", cpu: "x64" },
    { name: "linux-x64", os: "linux", cpu: "x64", libc: "glibc" },
    { name: "linux-arm64", os: "linux", cpu: "arm64", libc: "glibc" },
    { name: "win32-x64", os: "win32", cpu: "x64" },
  ];
}

function mainPackageVersion() {
  try {
    return JSON.parse(fs.readFileSync(path.join(PACKAGE_DIR, "package.json"), "utf8")).version;
  } catch {
    return null;
  }
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
    return !process.report?.getReport()?.header?.glibcVersionRuntime;
  } catch {
    return false;
  }
}

/**
 * 是否跑在 WSL 里。
 *
 * WSL 的 process.platform 是 linux + process.arch 与宿主一致，用 linux 的 glibc 包是对的；
 * 但 /mnt/c 下的 Windows 版 Node 会报 win32 —— 那种组合装出来的二进制在 WSL 里跑不起来。
 * 这里只把结论放进报错信息，不改查找逻辑（改错方向比不改更糟）。
 */
function isWsl() {
  if (process.platform !== "linux") return false;
  try {
    return /microsoft/i.test(fs.readFileSync("/proc/version", "utf8"));
  } catch {
    return false;
  }
}

/**
 * 当前平台对应的预编译子包名；没有预编译包时返回 null。
 *
 * 平台清单来自 scripts/platforms.json（不再在这里硬编码）。win32-arm64（Windows on ARM）
 * 与 Alpine/musl 没有预编译包——发一个从未在 CI 构建过的包比不发布更糟，所以返回 null，
 * 由 resolveBinary 给出「自编译」的可操作提示，而不是抛 Unsupported platform 让用户无处可去。
 * ATB_PLATFORM_OVERRIDE 可以强制指定平台 key（交叉安装 / 容器内排障用）。
 */
function platformPackageName(platform = process.platform, arch = process.arch) {
  const baseName = (() => {
    try {
      return JSON.parse(fs.readFileSync(path.join(PACKAGE_DIR, "package.json"), "utf8")).name;
    } catch {
      return "@g1728578755/atb";
    }
  })();
  const platforms = loadPlatforms();
  const override = process.env.ATB_PLATFORM_OVERRIDE;
  if (override) {
    const hit = platforms.find((item) => item.name === override);
    return hit ? `${baseName}-${hit.name}` : null;
  }
  if (platform === "linux" && isMusl()) return null;
  const hit = platforms.find((item) => item.os === platform && item.cpu === arch);
  return hit ? `${baseName}-${hit.name}` : null;
}

/** 平台包版本必须与主包一致：npm 的 optionalDependencies 语义就是同版本，混装只会跑错二进制。 */
function assertPlatformVersion(pkgName, binaryPath) {
  const expected = mainPackageVersion();
  if (!expected) return;
  let dir = path.dirname(binaryPath);
  let actual = null;
  for (let depth = 0; depth < 5 && dir !== path.dirname(dir); depth += 1) {
    const candidate = path.join(dir, "package.json");
    if (fs.existsSync(candidate)) {
      try {
        const data = JSON.parse(fs.readFileSync(candidate, "utf8"));
        if (data.name === pkgName) {
          actual = data.version;
          break;
        }
      } catch {
        // 继续往上找
      }
    }
    dir = path.dirname(dir);
  }
  if (actual === null || actual === expected) return;
  const message =
    `atb 原生二进制版本不匹配：主包 ${expected}，平台包 ${pkgName} 是 ${actual}。` +
    `\n这通常是多个版本混装或全局/局部 node_modules 串了。` +
    `\n修复：npm install -g @g1728578755/atb@${expected} --force（不要用 --omit=optional）。` +
    `\n确认无误要坚持运行：设置 ATB_ALLOW_VERSION_MISMATCH=1。`;
  if (process.env.ATB_ALLOW_VERSION_MISMATCH === "1") {
    console.error(`[atb] 警告：${message}`);
    return;
  }
  throw new Error(message);
}

/** PATH 里找 atb（最后兜底），跳过指向本 launcher 的 shim，避免自调用成环。 */
function findOnPath(expectedVersion) {
  // 不知道主包版本时不要用 PATH：无法确认 PATH 里那个 atb 是不是同一版本，
  // 静默用上一个旧版本比直接报错更难排查。
  if (!expectedVersion) return null;
  const command = process.platform === "win32" ? "where" : "which";
  const result = spawnSync(command, ["atb"], { encoding: "utf8" });
  if (result.status !== 0 || !result.stdout) return null;
  for (const line of result.stdout.split(/\r?\n/)) {
    const candidate = line.trim();
    if (!candidate || !fs.existsSync(candidate)) continue;
    if (path.resolve(candidate) === path.resolve(__filename)) continue;
    // npm 的 atb / atb.cmd / atb.ps1 shim 是文本包装，里面写着 atb.js；真的原生二进制不是。
    try {
      const fd = fs.openSync(candidate, "r");
      const head = Buffer.alloc(512);
      const read = fs.readSync(fd, head, 0, 512, 0);
      fs.closeSync(fd);
      const text = head.subarray(0, read).toString("utf8");
      if (text.includes("atb.js") || text.includes("npm/bin")) continue;
    } catch {
      continue;
    }
    // 版本对不上就不用：PATH 里可能是另一个版本（甚至是已废弃的旧 daemon 客户端）。
    const probe = spawnSync(candidate, ["--version"], { encoding: "utf8", timeout: 5000 });
    if (probe.status !== 0) continue;
    if (!String(probe.stdout || "").includes(expectedVersion)) continue;
    return candidate;
  }
  return null;
}

function resolveBinary() {
  // 1) 显式指定：先校验文件存在。以前直接把 env 值交给 spawn，用户只能看到
  // `spawn xxx ENOENT`，不知道是自己的环境变量写错了。
  for (const envName of ["ATB_BIN", "AGENT_BROWSER_CLI_BIN"]) {
    const value = process.env[envName];
    if (!value) continue;
    if (!fs.existsSync(value)) {
      throw new Error(
        `${envName}=${value} 指向的文件不存在。` +
          `\n修正该环境变量，或删掉它让 atb 自动查找原生二进制（npm install -g @g1728578755/atb --force）。`,
      );
    }
    return value;
  }

  const ext = process.platform === "win32" ? ".exe" : "";
  const pkgName = platformPackageName();
  const tried = [];

  // 2) 预编译平台包（npm 安装路径），顺带校验平台包版本与主包一致。
  if (pkgName) {
    for (const name of BINARY_NAMES) {
      let target = null;
      try {
        target = require.resolve(`${pkgName}/bin/${name}${ext}`);
      } catch {
        continue; // 继续试下一个名字 / 走本地开发路径
      }
      if (!fs.existsSync(target)) {
        tried.push(`${target}（require.resolve 命中但文件缺失）`);
        continue;
      }
      assertPlatformVersion(pkgName, target);
      return target;
    }
  }

  // 3) 本地开发布局：<repo>/npm/bin → ../../target/release（cargo build --release 的产物）
  //    npm 安装布局：  <prefix>/node_modules/@g1728578755/atb/npm/bin
  //                    → <pkg>/target/release —— 发布包的 files 里没有 target，
  //                    所以全局安装时这里必然不存在，只是为了本地开发不依赖 resolve。
  for (const name of BINARY_NAMES) {
    const candidate = path.resolve(__dirname, "..", "..", "target", "release", `${name}${ext}`);
    tried.push(candidate);
    if (fs.existsSync(candidate)) return candidate;
  }

  // 4) PATH（用户自己 cargo install 或手工放到 PATH 里的情况）。
  const fromPath = findOnPath(mainPackageVersion());
  if (fromPath) return fromPath;

  // 没有预编译包的平台：不能只喊 Unsupported，必须给出可执行的自救路径。
  if (!pkgName) {
    const muslHint = isMusl()
      ? `\n  4. Alpine/musl 环境：官方预编译包依赖 glibc，需自行编译（cargo build --release）或改用 glibc 基础镜像（如 node:20-bookworm-slim）。`
      : "";
    const wslHint = isWsl()
      ? `\n  5. 已检测到 WSL：请确认装的是 Linux 版 Node（which node 指向 /usr/...），Windows 版 Node 在 WSL 里装出的二进制无法运行。`
      : "";
    const overrideHint = process.env.ATB_PLATFORM_OVERRIDE
      ? `\n  注意：ATB_PLATFORM_OVERRIDE=${process.env.ATB_PLATFORM_OVERRIDE} 不在 scripts/platforms.json 的平台清单里，` +
        `可用值：${loadPlatforms().map((item) => item.name).join(' / ')}`
      : "";
    throw new Error(
      `atb 未提供 ${process.platform}-${process.arch} 的预编译包。` +
        `目前已发布：macOS (arm64/x64)、Linux glibc (x64/arm64)、Windows (x64)。\n` +
        `可选做法：\n` +
        `  1. 自行编译：在仓库根目录执行 "cargo build --release"，产物为 target/release/atb${ext}；\n` +
        `  2. 指定二进制：设置环境变量 AGENT_BROWSER_CLI_BIN 或 ATB_BIN 指向该产物（建议写进 shell 配置，避免新终端失效）；\n` +
        `  3. Windows on ARM 可改装 x64 版 Node.js（系统会做 x64 仿真），此时 process.arch 为 x64，可直接使用预编译包。` +
        muslHint +
        wslHint +
        overrideHint,
    );
  }
  throw new Error(
    `atb 原生二进制缺失（可选依赖 ${pkgName}）。已尝试：${tried.join("、") || "(无)"}。` +
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

let bin;
try {
  bin = resolveBinary();
} catch (error) {
  console.error(error.message);
  process.exit(1);
}

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
