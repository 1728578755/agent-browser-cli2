// 平台子包版本对齐：package.json 是唯一真相源。
//
// 为什么需要这个脚本：
//   optionalDependencies 里 5 个平台包必须与主包同版本，否则 npm 会静默跳过该依赖，
//   用户"安装成功"却在运行时拿到 native binary not found。
//   package-lock.json 里同样要写全 version / resolved / integrity / os / cpu，
//   否则 `npm ci` 直接以 EUSAGE 失败（lock 版本停在 0.3.6、平台条目只有
//   {"optional":true}，没有任何 version 字段）。
//
// 用法：node scripts/sync-optional-deps.mjs    # prepack / npm version 钩子调用
//
// 注意：这里不写 resolved 之外的 integrity —— 新版本刚 bump 时平台包还没发布，
// 旧 tarball 的 integrity 已失效，留着会让 npm 校验失败；干脆删掉，由 npm 安装时重新计算。

import fs from "node:fs";

const packagePath = "package.json";
const lockPath = "package-lock.json";
const registry = "https://registry.npmjs.org";

const packageJson = JSON.parse(fs.readFileSync(packagePath, "utf8"));
const packageVersion = packageJson.version;

// 与 .github/workflows/publish.yml 的 matrix 一一对应。
// win32-arm64 故意不在其中：没有 CI 能验证的构建矩阵不如不承诺，
// 该平台走 README 里的"自行编译 + AGENT_BROWSER_CLI_BIN"路径。
const platforms = [
  { name: "agent-browser-cli2-darwin-arm64", os: "darwin", cpu: "arm64" },
  { name: "agent-browser-cli2-darwin-x64", os: "darwin", cpu: "x64" },
  { name: "agent-browser-cli2-linux-x64", os: "linux", cpu: "x64" },
  { name: "agent-browser-cli2-linux-arm64", os: "linux", cpu: "arm64" },
  { name: "agent-browser-cli2-windows-x64", os: "win32", cpu: "x64" }
];

packageJson.optionalDependencies ||= {};
for (const { name } of platforms) {
  packageJson.optionalDependencies[name] = packageVersion;
}

fs.writeFileSync(packagePath, JSON.stringify(packageJson, null, 2) + "\n", "utf8");

if (fs.existsSync(lockPath)) {
  const lockJson = JSON.parse(fs.readFileSync(lockPath, "utf8"));
  lockJson.version = packageVersion;

  lockJson.packages ||= {};
  const rootPackage = (lockJson.packages[""] ||= {});
  rootPackage.version = packageVersion;
  rootPackage.optionalDependencies ||= {};
  for (const { name } of platforms) {
    rootPackage.optionalDependencies[name] = packageVersion;
  }

  for (const { name, os, cpu } of platforms) {
    const key = `node_modules/${name}`;
    const entry = (lockJson.packages[key] ||= { optional: true });
    // 包名可能是 scope（@scope/name）或无 scope（name），tarball 文件名只取最后一段。
    const shortName = name.includes("/") ? name.split("/")[1] : name;
    const tarball = `${registry}/${name}/-/${shortName}-${packageVersion}.tgz`;
    // 版本变了 → resolved 指向的 tarball 变了 → 旧的 integrity 必然对不上
    if (entry.resolved !== tarball && entry.integrity) delete entry.integrity;
    entry.version = packageVersion;
    entry.resolved = tarball;
    entry.os = [os];
    entry.cpu = [cpu];
    entry.optional = true;
  }

  fs.writeFileSync(lockPath, JSON.stringify(lockJson, null, 2) + "\n", "utf8");
}
