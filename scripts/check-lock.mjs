#!/usr/bin/env node
// 构建期锁文件门禁。
//
// 为什么必须有这一步：`cargo build --release --locked` / `npm ci` 一旦发现锁文件与
// 清单不一致就**直接失败**，而失败点在流水线中段、信息量很低（"the lock file needs
// to be updated"）。真正的错因通常是有人改了 Cargo.toml 的版本或依赖、却忘了同步
// Cargo.lock / package-lock.json。把判定提前到构建之前，一条命令就能定位。
//
// 检查项（全部通过才算 OK）：
//   1. Cargo.lock 存在；其中 `name = "atb"` 的 version == Cargo.toml [package].version
//   2. Cargo.lock 里 atb 的 dependencies 集合 == Cargo.toml [dependencies] 的键集合
//      （`[target.'cfg(...)'.dependencies]` 与 `[features]` 不参与集合比较）
//   3. Cargo.lock 里每个直接依赖的版本满足 Cargo.toml 里的版本要求
//   4. package-lock.json 的根版本 == package.json
//   5. package-lock.json 的 5 个平台包条目：version / os / cpu / resolved / integrity
//      与 scripts/platforms.json + 当前版本一致
//
// 用法：node scripts/check-lock.mjs
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadConfig, platformPackageNames } from './platforms.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/// 去掉行尾注释；引号内的 `#` 不算注释（依赖的 url/feature 里可能有）。
function stripComment(line) {
  let quote = null;
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (quote) {
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (ch === '#') return line.slice(0, i);
  }
  return line;
}

function unquote(value) {
  const text = value.trim();
  const match = text.match(/^["'](.*)["']$/);
  return match ? match[1] : text;
}

/// 从 `{ version = "0.7", features = [...] }` 里取 version；没有就返回 undefined。
function inlineVersion(value) {
  const matched = /\bversion\s*=\s*["']([^"']+)["']/.exec(value);
  return matched ? matched[1] : undefined;
}

/// 只解析 `[package]` / `[dependencies]` / `[target.*.dependencies]` 三段——
/// 够用且不引第三方 TOML 依赖。target 段只用来把对应依赖名从锁文件里排除。
function parseCargoToml(text) {
  const manifest = {
    version: undefined,
    dependencies: new Set(),
    targetDependencies: new Set(),
    requirements: new Map(),
  };
  let section = null;
  for (const raw of text.split(/\r?\n/)) {
    const line = stripComment(raw).trim();
    if (!line) continue;
    const header = line.match(/^\[([^\]]+)\]$/);
    if (header) {
      section = header[1].trim();
      continue;
    }
    const isTargetDeps = section !== null && /^target\..*\.dependencies$/.test(section);
    if (section !== 'package' && section !== 'dependencies' && !isTargetDeps) continue;
    const eq = line.indexOf('=');
    if (eq <= 0) continue;
    const key = unquote(line.slice(0, eq).trim());
    if (!key) continue;
    if (section === 'package') {
      if (key === 'version') manifest.version = unquote(line.slice(eq + 1));
    } else {
      if (isTargetDeps) manifest.targetDependencies.add(key);
      else manifest.dependencies.add(key);
      const value = line.slice(eq + 1).trim();
      const requirement = value.startsWith('{') ? inlineVersion(value) : unquote(value);
      if (requirement && /^[~^=<>*0-9]/.test(requirement)) {
        manifest.requirements.set(key, requirement);
      }
    }
  }
  return manifest;
}

/// 解析 Cargo.lock：按 `[[package]]` 分块，保留全部 name → versions（同名可多版本）。
///
/// 依赖项形如 `"serde"` 或 `"windows-sys 0.60.2"`（同名多版本时 Cargo 会附版本号）——
/// 只取第一段作为包名，否则和 Cargo.toml 的键集合永远对不上。
function parseCargoLock(text) {
  const packages = [];
  let current = null;
  let inDependencies = false;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('[[')) {
      if (current) packages.push(current);
      current = { name: undefined, version: undefined, dependencies: [] };
      inDependencies = false;
      continue;
    }
    if (!current) continue;
    if (line === '') {
      inDependencies = false;
      continue;
    }
    if (line === 'dependencies = [') {
      inDependencies = true;
      continue;
    }
    if (inDependencies) {
      if (line === ']') {
        inDependencies = false;
        continue;
      }
      const dep = line.match(/^"([^"]+)",?$/);
      if (dep) current.dependencies.push(dep[1].split(/\s+/)[0]);
      continue;
    }
    const field = line.match(/^(name|version)\s*=\s*"([^"]*)"$/);
    if (field) current[field[1]] = field[2];
  }
  if (current) packages.push(current);
  return packages;
}

// ---------------------------------------------------------------------------
// 版本要求判定（够用于 Cargo 里常见的 "1.0" / "0.7" / "^1.2" / "~0.4" / "=1.0.0"）
// ---------------------------------------------------------------------------

function parseVersion(text) {
  const matched = /^(\d+)\.(\d+)\.(\d+)/.exec(String(text ?? '').trim());
  return matched ? [Number(matched[1]), Number(matched[2]), Number(matched[3])] : null;
}

function compareVersion(a, b) {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  return 0;
}

function satisfiesOne(version, requirement) {
  const text = String(requirement).trim().replace(/,/g, ' ');
  const matched = /^(\^|~|=|>=|<=|>|<)?\s*(\d+)(?:\.(\d+))?(?:\.(\d+))?\s*$/.exec(text);
  if (!matched) return true; // "*" / 无法解析的要求不判定，交给 cargo 自己报错
  const op = matched[1] ?? '^';
  const parts = [Number(matched[2]), Number(matched[3] ?? 0), Number(matched[4] ?? 0)];
  const precision = matched[4] !== undefined ? 3 : matched[3] !== undefined ? 2 : 1;
  const lower = parts;
  switch (op) {
    case '=':
      return compareVersion(version, parts) === 0;
    case '>':
      return compareVersion(version, parts) > 0;
    case '>=':
      return compareVersion(version, parts) >= 0;
    case '<':
      return compareVersion(version, parts) < 0;
    case '<=':
      return compareVersion(version, parts) <= 0;
    case '~': {
      const upper = precision > 1 ? [parts[0], parts[1] + 1, 0] : [parts[0] + 1, 0, 0];
      return compareVersion(version, lower) >= 0 && compareVersion(version, upper) < 0;
    }
    default: {
      // caret：^1.2.3 → <2.0.0；^0.3 → <0.4.0；^0.0.3 → <0.0.4；^0 → <1.0.0
      let upper;
      if (parts[0] > 0) upper = [parts[0] + 1, 0, 0];
      else if (parts[1] > 0 || precision > 2) upper = [0, parts[1] + 1, 0];
      else if (parts[2] > 0 || precision === 1) upper = [0, 1, 0];
      else upper = [0, 0, parts[2] + 1];
      return compareVersion(version, lower) >= 0 && compareVersion(version, upper) < 0;
    }
  }
}

function satisfies(versionText, requirement) {
  const version = parseVersion(versionText);
  if (!version) return true;
  return String(requirement)
    .split(/\s*,\s*/)
    .filter(Boolean)
    .every((one) => satisfiesOne(version, one));
}

function readJson(rel) {
  const { path } = { path: join(ROOT, rel) };
  const raw = readFileSync(path, 'utf8');
  return { path, data: JSON.parse(raw) };
}

function compareList(actual, expected) {
  if (!Array.isArray(actual)) return false;
  const left = [...actual].sort();
  const right = [...expected].sort();
  return left.length === right.length && left.every((item, index) => item === right[index]);
}

function checkPlatformPackages(pkg, pkgLock, platforms, registry) {
  const problems = [];
  const version = pkg.version;
  const expected = platformPackageNames(pkg.name, platforms);
  const declared = pkg.optionalDependencies ?? {};
  const rootPackage = pkgLock.packages?.[''] ?? {};
  const lockDeclared = rootPackage.optionalDependencies ?? {};

  for (const name of expected) {
    if (declared[name] === undefined) problems.push(`package.json optionalDependencies 缺少平台包 ${name}`);
    else if (declared[name] !== version) {
      problems.push(`package.json optionalDependencies["${name}"] = ${declared[name]}，应为 ${version}`);
    }
    if (lockDeclared[name] === undefined) problems.push(`package-lock.json 顶层 optionalDependencies 缺少 ${name}`);
    else if (lockDeclared[name] !== declared[name]) {
      problems.push(`package-lock.json optionalDependencies["${name}"] = ${lockDeclared[name]} ≠ package.json`);
    }
  }
  for (const name of Object.keys(declared)) {
    if (!expected.includes(name)) problems.push(`package.json optionalDependencies 多出未声明的平台包 ${name}`);
  }
  for (const name of Object.keys(lockDeclared)) {
    if (!expected.includes(name)) problems.push(`package-lock.json 多出未声明的平台包 ${name}`);
  }

  for (const platform of platforms) {
    const name = `${pkg.name}-${platform.name}`;
    const key = `node_modules/${name}`;
    const entry = pkgLock.packages?.[key];
    if (!entry) {
      problems.push(`package-lock.json 缺少平台包条目 ${key}`);
      continue;
    }
    if (entry.version !== version) {
      problems.push(`${key} 的 version = ${entry.version ?? '(缺失)'}，应为 ${version}`);
    }
    if (!compareList(entry.os, [platform.os])) {
      problems.push(`${key} 的 os = ${JSON.stringify(entry.os ?? null)}，应为 ${JSON.stringify([platform.os])}`);
    }
    if (!compareList(entry.cpu, [platform.cpu])) {
      problems.push(`${key} 的 cpu = ${JSON.stringify(entry.cpu ?? null)}，应为 ${JSON.stringify([platform.cpu])}`);
    }
    const tarball = `${registry}/${name}/-/${name.split('/')[1]}-${version}.tgz`;
    if (entry.resolved !== tarball) {
      problems.push(`${key} 的 resolved = ${entry.resolved ?? '(缺失)'}，应为 ${tarball}`);
    }
    if (entry.integrity === undefined) {
      // 刚 bump 完、还没重新 npm install 时新 tarball 的 integrity 无法离线算出，
      // 这是唯一合法缺失场景（resolved 已指向当前版本）。其余缺失说明 lock 被手改过。
      const resolvedIsCurrent = typeof entry.resolved === 'string' && entry.resolved.includes(`-${version}.tgz`);
      if (!resolvedIsCurrent && process.env.ATB_LOCK_ALLOW_MISSING_INTEGRITY !== '1') {
        problems.push(`${key} 的 integrity 缺失（resolved 也不是当前版本 ${version}）`);
      }
    } else if (!/^sha(1|512)-[A-Za-z0-9+/=]+$/.test(entry.integrity)) {
      problems.push(`${key} 的 integrity 格式非法: ${entry.integrity}`);
    }
  }
  return problems;
}

function main() {
  const problems = [];

  const cargoTomlPath = join(ROOT, 'Cargo.toml');
  const cargoLockPath = join(ROOT, 'Cargo.lock');
  const packagePath = join(ROOT, 'package.json');
  const packageLockPath = join(ROOT, 'package-lock.json');

  if (!existsSync(cargoLockPath)) {
    problems.push('Cargo.lock 不存在：--locked 构建会直接失败，请先 cargo generate-lockfile');
    report(problems);
    return;
  }

  const manifest = parseCargoToml(readFileSync(cargoTomlPath, 'utf8'));
  if (!manifest.version) problems.push('Cargo.toml 的 [package] 段缺少 version');

  const packages = parseCargoLock(readFileSync(cargoLockPath, 'utf8'));
  const locked = packages.find((pkg) => pkg.name === 'atb') ?? null;
  const lockedVersions = new Map();
  for (const item of packages) {
    if (!item.name || !item.version) continue;
    if (!lockedVersions.has(item.name)) lockedVersions.set(item.name, []);
    lockedVersions.get(item.name).push(item.version);
  }

  if (!locked) {
    problems.push('Cargo.lock 里找不到 name = "atb" 的包');
  } else {
    if (manifest.version && locked.version !== manifest.version) {
      problems.push(
        `Cargo.lock 的 atb version = ${locked.version}，Cargo.toml = ${manifest.version}（版本漂移会让 --locked 构建失败）`,
      );
    }
    // Cargo.lock 是跨平台合并结果：`[target.'cfg(...)'.dependencies]` 的依赖也会出现在
    // atb 的 dependencies 列表里，按任务约定它们不参与集合比较，先排除掉。
    const lockedDeps = new Set(locked.dependencies.filter((name) => !manifest.targetDependencies.has(name)));
    const missing = [...manifest.dependencies].filter((name) => !lockedDeps.has(name)).sort();
    const extra = [...lockedDeps].filter((name) => !manifest.dependencies.has(name)).sort();
    if (missing.length) {
      problems.push(`Cargo.lock 缺少依赖: ${missing.join(', ')}（改了 Cargo.toml 却没更新锁文件）`);
    }
    if (extra.length) {
      problems.push(`Cargo.lock 多出依赖: ${extra.join(', ')}（Cargo.toml 已删除，锁文件未更新）`);
    }
  }

  // 依赖版本也要校验。只比名字时，`serde = "1.0"` 在 lock 里退到 0.9 也能过，
  // 构建期才会以「版本要求不满足」失败，而且报错在几十条编译日志之后。
  for (const [name, requirement] of manifest.requirements) {
    const versions = lockedVersions.get(name);
    if (!versions || versions.length === 0) continue; // 缺失已在上面报过
    if (!versions.some((version) => satisfies(version, requirement))) {
      problems.push(
        `Cargo.lock 里 ${name} 的版本 ${versions.join(' / ')} 不满足 Cargo.toml 要求 "${requirement}"（锁文件过期）`,
      );
    }
  }

  const pkg = readJson('package.json').data;
  const pkgLock = readJson('package-lock.json').data;
  const lockedVersion = pkgLock?.packages?.['']?.version;
  if (!lockedVersion) {
    problems.push('package-lock.json 缺少 packages[""].version');
  } else if (lockedVersion !== pkg.version) {
    problems.push(`package-lock.json 的 packages[""].version = ${lockedVersion}，package.json = ${pkg.version}`);
  }
  if (existsSync(packageLockPath)) {
    const { registry, platforms } = loadConfig();
    problems.push(...checkPlatformPackages(pkg, pkgLock, platforms, registry));
  }

  report(problems);
}

function report(problems) {
  if (problems.length === 0) {
    console.log('lock 一致');
    return;
  }
  console.error('lock 不一致：');
  for (const problem of problems) console.error(`  - ${problem}`);
  console.error('修复：版本号用 `npm run sync-version` 同步；依赖变动后运行 `cargo generate-lockfile`。');
  process.exit(1);
}

try {
  main();
} catch (error) {
  console.error(`lock 检查执行失败: ${error.message}`);
  process.exit(1);
}
