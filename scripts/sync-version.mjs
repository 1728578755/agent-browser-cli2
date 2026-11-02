#!/usr/bin/env node
// 版本同步与校验：以 package.json 为唯一真相源，把同一个版本写进 Cargo.toml 与 README。
//
// 用法：
//   node scripts/sync-version.mjs                    # 把 package.json 的版本写入 Cargo.toml / README 并 git add
//   node scripts/sync-version.mjs --check            # 校验 package.json / Cargo.toml / README / package-lock.json
//   node scripts/sync-version.mjs --check --expect 0.3.7   # 再把 tag 版本一并校验
//   node scripts/sync-version.mjs --cargo native/Cargo.toml
//
// 退出码：0 一致 / 已写入；1 不一致或文件缺失，CI 直接失败。

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const CANDIDATES = ['Cargo.toml', 'native/Cargo.toml', 'rust/Cargo.toml'];

// README 里版本号的两个落点：徽章（`release-v0.3.7-orange` / `alt="release v0.3.7"`）
// 与版本行（中文 `- 当前版本：` / 英文 `- Current version: `）。
// README_EN.md 曾经滞后两个版本，所以两者都纳入同步与校验。
const BADGE_VERSION = /release([- ])v\d+\.\d+\.\d+/g;
const VERSION_LINES = [
  /(-\s*当前版本[：:]\s*`)([^`]+)(`)/,
  /(-\s*Current version:\s*`)([^`]+)(`)/,
];
const README_FILES = ['README.md', 'README_EN.md'];

function parseArgs(argv) {
  const args = { check: false, expect: null, cargo: null };
  for (let i = 0; i < argv.length; i += 1) {
    const current = argv[i];
    if (current === '--check') args.check = true;
    else if (current === '--expect') args.expect = argv[++i] ?? null;
    else if (current === '--cargo') args.cargo = argv[++i] ?? null;
    else if (current === '--help' || current === '-h') {
      console.log('用法: sync-version.mjs [--check] [--expect <version>] [--cargo <path>]');
      process.exit(0);
    } else throw new Error(`未知参数: ${current}`);
  }
  return args;
}

// 定位仓库根目录与 Cargo.toml：脚本位于 <root>/scripts/ 下
// fileURLToPath 是必须的：Windows 上 `new URL(...).pathname` 返回 `/D:/...`，
// resolve() 会把它当相对路径拼成 `D:\D:\...`。
function resolveRoot() {
  return resolve(dirname(fileURLToPath(import.meta.url)), '..');
}

function findCargo(root, explicit) {
  if (explicit) {
    const path = resolve(root, explicit);
    if (!existsSync(path)) throw new Error(`找不到 Cargo.toml: ${explicit}`);
    return path;
  }
  for (const rel of CANDIDATES) {
    const path = join(root, rel);
    if (existsSync(path)) return path;
  }
  throw new Error(`在 ${CANDIDATES.join(' / ')} 中都找不到 Cargo.toml`);
}

// 只改 [package] 段内的 version，避免误伤依赖版本
function readPackageVersion(lines) {
  const sectionStart = lines.findIndex((line) => /^\[package\]\s*$/.test(line));
  if (sectionStart === -1) throw new Error('Cargo.toml 缺少 [package] 段');
  const sectionEnd = lines.findIndex((line, index) => index > sectionStart && /^\[/.test(line));
  const limit = sectionEnd === -1 ? lines.length : sectionEnd;
  for (let i = sectionStart + 1; i < limit; i += 1) {
    const matched = lines[i].match(/^\s*version\s*=\s*"([^"]+)"/);
    if (matched) return { value: matched[1], lineIndex: i };
  }
  throw new Error('Cargo.toml 的 [package] 段内缺少 version');
}

function rewriteReadme(text, version) {
  let next = text.replace(BADGE_VERSION, (_match, sep) => `release${sep}v${version}`);
  for (const pattern of VERSION_LINES) {
    next = next.replace(pattern, (_match, head, _old, tail) => `${head}${version}${tail}`);
  }
  return next;
}

// 把 README 里出现过的版本号汇总出来，用于报错信息（不参与判断，判断看文本是否被改写）
function readmeVersions(text) {
  const found = new Set();
  for (const matched of text.matchAll(/release[- ]v?(\d+\.\d+\.\d+)/g)) found.add(matched[1]);
  for (const pattern of VERSION_LINES) {
    const matched = text.match(pattern);
    if (matched) found.add(matched[2]);
  }
  return [...found];
}

// lock 的一致性校验：npm ci 在 version 漂移时会直接 EUSAGE。
// 这里只读不写——lock 的写入统一由 scripts/sync-optional-deps.mjs 负责，
// 避免两个脚本同时改同一批字段产生顺序依赖。
function checkLock(root, version) {
  const lockPath = join(root, 'package-lock.json');
  if (!existsSync(lockPath)) return [];
  const lock = JSON.parse(readFileSync(lockPath, 'utf8'));
  const problems = [];
  if (lock.version !== version) {
    problems.push(`package-lock.json 顶层 version = ${lock.version ?? '(缺失)'}，应为 ${version}`);
  }
  const rootPackage = lock.packages?.[''] ?? {};
  if (rootPackage.version !== version) {
    problems.push(`package-lock.json 中 packages[""].version = ${rootPackage.version ?? '(缺失)'}，应为 ${version}`);
  }
  for (const name of Object.keys(rootPackage.optionalDependencies ?? {})) {
    const entry = lock.packages?.[`node_modules/${name}`];
    if (!entry) {
      problems.push(`package-lock.json 缺少平台包条目 ${name}`);
      continue;
    }
    if (entry.version !== version) {
      problems.push(`${name} 在 package-lock.json 中的 version = ${entry.version ?? '(缺失)'}，应为 ${version}`);
    }
  }
  return problems;
}

function gitAdd(root, paths) {
  try {
    execFileSync('git', ['add', ...paths], { cwd: root, stdio: 'pipe' });
    console.log(`已 git add ${paths.join(' ')}`);
  } catch {
    console.log(`跳过 git add（${root} 不是 git 仓库或 git 不可用）`);
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const root = resolveRoot();

  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  const packageVersion = pkg.version;
  if (!packageVersion) throw new Error('package.json 缺少 version');

  const cargoPath = findCargo(root, args.cargo);
  const cargoRel = cargoPath.slice(root.length + 1);
  const original = readFileSync(cargoPath, 'utf8');
  const lines = original.split('\n');
  const { value: cargoVersion, lineIndex } = readPackageVersion(lines);

  const mismatches = [];
  if (cargoVersion !== packageVersion) {
    mismatches.push(`${cargoRel} 中的 version = ${cargoVersion} ≠ package.json = ${packageVersion}`);
  }
  if (args.expect && args.expect !== packageVersion) {
    mismatches.push(`期望版本 = ${args.expect} ≠ package.json = ${packageVersion}`);
  }

  const readmeChanges = [];
  for (const file of README_FILES) {
    const path = join(root, file);
    if (!existsSync(path)) continue;
    const text = readFileSync(path, 'utf8');
    const next = rewriteReadme(text, packageVersion);
    if (next !== text) {
      const found = readmeVersions(text);
      readmeChanges.push({ file, next, found });
      mismatches.push(`${file} 中的版本号 ${found.join(' / ') || '(未找到)'} ≠ package.json = ${packageVersion}`);
    }
  }

  const lockProblems = checkLock(root, packageVersion);

  if (mismatches.length === 0 && lockProblems.length === 0) {
    console.log(`版本一致: ${packageVersion}（${cargoRel} / ${README_FILES.join(' / ')} / package-lock.json）`);
    return;
  }

  if (args.check) {
    for (const line of mismatches) console.error(`版本不一致: ${line}`);
    for (const line of lockProblems) console.error(`版本不一致: ${line}`);
    if (lockProblems.length > 0) {
      console.error('运行 `node scripts/sync-optional-deps.mjs` 重新对齐 package-lock.json 后重试');
    }
    if (mismatches.length > 0) {
      console.error('运行 `npm run sync-version` 自动对齐后重试');
    }
    process.exit(1);
  }

  // 写入模式：只重写那行 version，保留其余内容与行尾
  if (cargoVersion !== packageVersion) {
    const indent = lines[lineIndex].match(/^\s*/)[0];
    lines[lineIndex] = `${indent}version = "${packageVersion}"`;
    writeFileSync(cargoPath, lines.join('\n'));
    console.log(`已把 ${cargoRel} 的版本从 ${cargoVersion} 改为 ${packageVersion}`);
  }
  for (const { file, next, found } of readmeChanges) {
    writeFileSync(join(root, file), next);
    console.log(`已把 ${file} 的版本从 ${found.join(' / ')} 改为 ${packageVersion}`);
  }
  const written = [];
  if (cargoVersion !== packageVersion) written.push(cargoRel);
  written.push(...readmeChanges.map(({ file }) => file));
  if (written.length > 0) gitAdd(root, written);

  if (args.expect && args.expect !== packageVersion) {
    console.error(`注意: 期望版本 = ${args.expect} ≠ package.json = ${packageVersion}`);
  }

  if (lockProblems.length > 0) {
    for (const line of lockProblems) console.error(`版本不一致: ${line}`);
    console.error('package-lock.json 不由本脚本写入，请运行 `node scripts/sync-optional-deps.mjs`');
    process.exit(1);
  }
}

try {
  main();
} catch (error) {
  console.error(`版本同步失败: ${error.message}`);
  process.exit(1);
}
