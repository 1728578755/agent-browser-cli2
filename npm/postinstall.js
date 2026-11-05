// npm postinstall：只做两件事——给 launcher 加执行位、确认原生二进制真的在。
//
// 旧实现只 chmod，且 `try { chmod } catch {}` 吞掉所有错误：
//   `npm i -g @g1728578755/atb --omit=optional`、离线缓存缺子包、平台包没发布……
//   都会「安装成功」，用户直到第一次运行才看到 native binary not found，
//   而那时既不知道原因也不知道怎么修。这里改成安装期就给出可执行的结论。
//
// 逃生口：ATB_SKIP_POSTINSTALL_CHECK=1 跳过探测（CI 里只装包不跑命令时用）。
// 退出码：0 找到二进制；1 找不到（npm 会把这个包的安装标成失败，这就是目的）。
const fs = require("node:fs");
const path = require("node:path");

const SKIP = process.env.ATB_SKIP_POSTINSTALL_CHECK === "1";
const LAUNCHER = path.resolve(__dirname, "bin", "atb.js");
const BINARY_NAMES = ["atb"];

function platformConfig() {
  const configPath = path.resolve(__dirname, "..", "scripts", "platforms.json");
  try {
    return JSON.parse(fs.readFileSync(configPath, "utf8"));
  } catch {
    // 旧的安装包没有 scripts/platforms.json：退回最小内置表（只为给出可操作的报错）。
    return {
      platforms: [
        { name: "darwin-arm64", os: "darwin", cpu: "arm64" },
        { name: "darwin-x64", os: "darwin", cpu: "x64" },
        { name: "linux-x64", os: "linux", cpu: "x64", libc: "glibc" },
        { name: "linux-arm64", os: "linux", cpu: "arm64", libc: "glibc" },
        { name: "win32-x64", os: "win32", cpu: "x64" },
      ],
    };
  }
}

function isMusl() {
  if (process.platform !== "linux") return false;
  try {
    return !process.report?.getReport()?.header?.glibcVersionRuntime;
  } catch {
    return false;
  }
}

/// 当前平台对应的子包名；不支持时返回 { name: null, reason }。
function resolvePlatformPackage(baseName) {
  const config = platformConfig();
  const platforms = config.platforms ?? [];
  const override = process.env.ATB_PLATFORM_OVERRIDE;
  if (override) {
    const hit = platforms.find((item) => item.name === override);
    if (!hit) {
      return { name: null, reason: `ATB_PLATFORM_OVERRIDE=${override} 不在 scripts/platforms.json 里` };
    }
    return { name: `${baseName}-${hit.name}`, platform: hit };
  }
  if (process.platform === "linux" && isMusl()) {
    return { name: null, reason: "Alpine/musl 环境没有预编译包（官方包依赖 glibc）" };
  }
  const hit = platforms.find((item) => item.os === process.platform && item.cpu === process.arch);
  if (!hit) return { name: null, reason: `没有 ${process.platform}-${process.arch} 的预编译包` };
  return { name: `${baseName}-${hit.name}`, platform: hit };
}

function main() {
  // 1) 执行位：Windows 不需要，其它平台失败也不致命（用户可直接 node npm/bin/atb.js）。
  try {
    fs.chmodSync(LAUNCHER, 0o755);
  } catch (error) {
    if (process.platform !== "win32") {
      console.warn(`[atb] 无法给 ${LAUNCHER} 加执行位：${error.message}`);
    }
  }

  // 2) 二进制探测：只读，不改任何文件。
  const rootPackage = JSON.parse(fs.readFileSync(path.resolve(__dirname, "..", "package.json"), "utf8"));
  const ext = process.platform === "win32" ? ".exe" : "";
  const hints = [];

  for (const envName of ["ATB_BIN", "AGENT_BROWSER_CLI_BIN"]) {
    const value = process.env[envName];
    if (!value) continue;
    if (fs.existsSync(value)) {
      console.log(`[atb] 使用 ${envName} 指定的二进制：${value}`);
      return 0;
    }
    hints.push(`${envName} 指向的文件不存在：${value}`);
  }

  const resolved = resolvePlatformPackage(rootPackage.name);
  if (resolved.name) {
    for (const binaryName of BINARY_NAMES) {
      try {
        const target = require.resolve(`${resolved.name}/bin/${binaryName}${ext}`);
        if (fs.statSync(target).size === 0) {
          hints.push(`${resolved.name}/bin/${binaryName}${ext} 是空文件`);
          continue;
        }
        console.log(`[atb] 原生二进制就绪：${target}`);
        return 0;
      } catch {
        // 继续试下一个名字 / 走下面的报错
      }
    }
    hints.push(`可选依赖 ${resolved.name} 没有装上它的 bin/${BINARY_NAMES[0]}${ext}`);
  } else {
    hints.push(resolved.reason);
  }

  if (SKIP) {
    console.warn("[atb] 未找到原生二进制（ATB_SKIP_POSTINSTALL_CHECK=1，跳过检查）");
    for (const hint of hints) console.warn(`  - ${hint}`);
    return 0;
  }

  console.error("");
  console.error("[atb] 安装完成，但没有找到可执行的 atb 原生二进制。");
  for (const hint of hints) console.error(`  - ${hint}`);
  console.error("");
  console.error("可选做法：");
  console.error("  1. 重装（不要用 --omit=optional / --no-optional / --ignore-scripts）：npm install -g @g1728578755/atb@latest --force");
  console.error("  2. 指定已有二进制：设置 ATB_BIN 或 AGENT_BROWSER_CLI_BIN 指向它（建议写进 shell 配置）");
  console.error("  3. 自行编译：仓库根目录 cargo build --release，产物在 target/release/atb");
  console.error("  4. 只是离线打包、不需要运行时：设置 ATB_SKIP_POSTINSTALL_CHECK=1");
  console.error("");
  return 1;
}

process.exitCode = main();
