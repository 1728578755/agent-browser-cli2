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
//   2. package-lock.json 的 packages[""].version == package.json 的 version
//   3. Cargo.lock 里 atb 的 dependencies 集合 == Cargo.toml [dependencies] 的键集合
//      （`[target.'cfg(...)'.dependencies]` 与 `[features]` 不参与比较）
//
// 用法：node scripts/check-lock.mjs
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

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

/// 只解析 `[package]` / `[dependencies]` / `[target.*.dependencies]` 三段——
/// 够用且不引第三方 TOML 依赖。target 段只用来把对应依赖名从锁文件里排除。
function parseCargoToml(text) {
  const manifest = { version: undefined, dependencies: new Set(), targetDependencies: new Set() };
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
    } else if (isTargetDeps) {
      manifest.targetDependencies.add(key);
    } else {
      manifest.dependencies.add(key);
    }
  }
  return manifest;
}

/// 解析 Cargo.lock：按 `[[package]]` 分块，取 name 为 atb 的那块的版本与依赖名。
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
  return packages.find((pkg) => pkg.name === 'atb') ?? null;
}

function readJson(rel) {
  const path = join(ROOT, rel);
  const raw = readFileSync(path, 'utf8');
  return { path, data: JSON.parse(raw) };
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

  const locked = parseCargoLock(readFileSync(cargoLockPath, 'utf8'));
  if (!locked) {
    problems.push('Cargo.lock 里找不到 name = "atb" 的包');
  } else {
    if (manifest.version && locked.version !== manifest.version) {
      problems.push(
        `Cargo.lock 的 atb version = ${locked.version}，Cargo.toml = ${manifest.version}（版本漂移会让 --locked 构建失败）`,
      );
    }
    // Cargo.lock 是跨平台合并结果：`[target.'cfg(...)'.dependencies]` 的依赖也会出现在
    // atb 的 dependencies 列表里，按任务约定它们不参与比较，先排除掉。
    const lockedDeps = new Set(
      locked.dependencies.filter((name) => !manifest.targetDependencies.has(name)),
    );
    const missing = [...manifest.dependencies].filter((name) => !lockedDeps.has(name)).sort();
    const extra = [...lockedDeps].filter((name) => !manifest.dependencies.has(name)).sort();
    if (missing.length) {
      problems.push(`Cargo.lock 缺少依赖: ${missing.join(', ')}（改了 Cargo.toml 却没更新锁文件）`);
    }
    if (extra.length) {
      problems.push(`Cargo.lock 多出依赖: ${extra.join(', ')}（Cargo.toml 已删除，锁文件未更新）`);
    }
  }

  const pkg = readJson('package.json').data;
  const pkgLock = readJson('package-lock.json').data;
  const lockedVersion = pkgLock?.packages?.['']?.version;
  if (!lockedVersion) {
    problems.push('package-lock.json 缺少 packages[""].version');
  } else if (lockedVersion !== pkg.version) {
    problems.push(
      `package-lock.json 的 packages[""].version = ${lockedVersion}，package.json = ${pkg.version}`,
    );
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
