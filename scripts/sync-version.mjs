#!/usr/bin/env node
// 版本同步与校验：以 package.json 为唯一真相源，把同一个版本写进 Cargo.toml 与 README。
//
// 用法：
//   node scripts/sync-version.mjs                    # 把 package.json 的版本写入 Cargo.toml / README，
//                                                    # 用 cargo 刷新 Cargo.lock，然后 git add
//   node scripts/sync-version.mjs --check            # 校验 package.json / Cargo.toml / README / package-lock.json
//   node scripts/sync-version.mjs --check --expect 0.3.7   # 再把 tag 版本一并校验
//   node scripts/sync-version.mjs --cargo native/Cargo.toml
//
// 写入模式是事务式的：Cargo.toml / README 任一写入或 Cargo.lock 刷新失败，都会把已经
// 改过的文件按原内容写回。
//
// 退出码：0 一致 / 已写入；1 不一致或文件缺失，CI 直接失败。

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
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
// resolve 会把它当相对路径拼成 `D:\D:\...`（v5）。
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
function readPackageField(lines, field) {
  const sectionStart = lines.findIndex((line) => /^\[package\]\s*$/.test(line));
  if (sectionStart === -1) throw new Error('Cargo.toml 缺少 [package] 段');
  const sectionEnd = lines.findIndex((line, index) => index > sectionStart && /^\[/.test(line));
  const limit = sectionEnd === -1 ? lines.length : sectionEnd;
  const pattern = new RegExp(`^\\s*${field}\\s*=\\s*"([^"]+)"`);
  for (let i = sectionStart + 1; i < limit; i += 1) {
    const matched = lines[i].match(pattern);
    if (matched) return { value: matched[1], lineIndex: i };
  }
  return null;
}

function readPackageVersion(lines) {
  const field = readPackageField(lines, 'version');
  if (!field) throw new Error('Cargo.toml 的 [package] 段内缺少 version');
  return field;
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
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

// lock 的一致性校验：npm ci 在 version 漂移时会直接 EUSAGE（v5）。
// 这里只读不写——lock 的写入统一由 scripts/sync-optional-deps.mjs 负责，
// 避免两个脚本同时改同一批字段产生顺序依赖。
//
// v6 P3：旧实现只遍历 lock 里 optionalDependencies 的**键**，package.json 那 5 个
// 平台包的**值**从未被读过。于是 `0.7.1` 里混进一个 `0.7.0` 的平台包也能过检查，
// 下游 npm ci 才会炸。现在键与值都校验，且反向检查 lock 里多余的平台包。
function checkLock(root, version, optionalDeps = {}) {
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
  const declared = optionalDeps ?? {};
  const lockDeclared = rootPackage.optionalDependencies ?? {};
  for (const [name, declaredVersion] of Object.entries(declared)) {
    if (declaredVersion !== version) {
      problems.push(`package.json optionalDependencies["${name}"] = ${declaredVersion}，应为 ${version}`);
    }
    if (lockDeclared[name] === undefined) {
      problems.push(`package-lock.json 顶层 optionalDependencies 缺少 ${name}`);
    } else if (lockDeclared[name] !== declaredVersion) {
      problems.push(
        `package-lock.json optionalDependencies["${name}"] = ${lockDeclared[name]} ≠ package.json = ${declaredVersion}`,
      );
    }
    const entry = lock.packages?.[`node_modules/${name}`];
    if (!entry) {
      problems.push(`package-lock.json 缺少平台包条目 ${name}`);
      continue;
    }
    if (entry.version !== declaredVersion) {
      problems.push(`${name} 在 package-lock.json 中的 version = ${entry.version ?? '(缺失)'}，应为 ${declaredVersion}`);
    }
  }
  for (const name of Object.keys(lockDeclared)) {
    if (!(name in declared)) {
      problems.push(`package-lock.json 中多出 package.json 未声明的平台包 ${name}`);
    }
  }
  return problems;
}

// Cargo.lock 也要跟着 package.json 走：`cargo build --locked` 以 lock 为准，
// 漏改会让下游构建直接失败（v6 P3）。这里只校验根包，不碰依赖树。
//
// `sourceLabel` 只影响报错措辞：写入前的自检比较的是「lock vs 当前 Cargo.toml」，
// 说成「≠ package.json」会把人引向错误的文件。
function checkCargoLock(cargoPath, packageName, version, sourceLabel = 'package.json') {
  const lockPath = join(dirname(cargoPath), 'Cargo.lock');
  if (!existsSync(lockPath)) return [];
  const text = readFileSync(lockPath, 'utf8');
  const blocks = text.split(/^\[\[package\]\]\s*$/m);
  const namePattern = new RegExp(`^name\\s*=\\s*"${escapeRegExp(packageName)}"\\s*$`, 'm');
  for (const block of blocks) {
    if (!namePattern.test(block)) continue;
    const matched = /^version\s*=\s*"([^"]+)"\s*$/m.exec(block);
    if (!matched) return [`Cargo.lock 中 ${packageName} 缺少 version`];
    if (matched[1] !== version) {
      return [`Cargo.lock 中 ${packageName} 的 version = ${matched[1]} ≠ ${sourceLabel} = ${version}`];
    }
    return [];
  }
  return [`Cargo.lock 中找不到根包 ${packageName}`];
}

function gitAdd(root, paths) {
  try {
    execFileSync('git', ['add', ...paths], { cwd: root, stdio: 'pipe' });
    console.log(`已 git add ${paths.join(' ')}`);
  } catch {
    console.log(`跳过 git add（${root} 不是 git 仓库或 git 不可用）`);
  }
}

// 改完 Cargo.toml 后让 cargo 自己把 Cargo.lock 的根包版本写回去。
//
// 手写 lock 看起来更快，但 lock 里还有 checksum / 依赖图等 cargo 自己维护的字段，
// 一旦格式跟着改版就会写出 cargo 不认的文件。这里只用「官方命令 + 事后校验」：
// `cargo update -p <包名> --precise <版本>` 是唯一能把根包钉到指定版本的写法。
// 失败返回错误摘要（调用方据此回滚），成功返回 null。
function refreshCargoLock(cargoPath, packageName, version) {
  const cwd = dirname(cargoPath);
  const attempts = [
    ['update', '-p', packageName, '--precise', version],
    // 无网环境（CI 容器、离线开发机）里第一次会卡在索引更新，退回离线模式再试一次。
    ['update', '-p', packageName, '--precise', version, '--offline'],
  ];
  let lastError = null;
  for (const args of attempts) {
    try {
      execFileSync('cargo', args, { cwd, stdio: 'pipe' });
      return null;
    } catch (error) {
      lastError = error;
    }
  }
  const detail = String(lastError?.stderr ?? lastError?.message ?? lastError).trim();
  return detail.split('\n').slice(0, 6).join('\n') || 'cargo 不可用';
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
  const cargoName = readPackageField(lines, 'name')?.value;
  if (!cargoName) throw new Error(`${cargoRel} 的 [package] 段内缺少 name`);

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

  const lockProblems = [
    ...checkLock(root, packageVersion, pkg.optionalDependencies),
    ...checkCargoLock(cargoPath, cargoName, packageVersion),
  ];

  if (mismatches.length === 0 && lockProblems.length === 0) {
    console.log(
      `版本一致: ${packageVersion}（${cargoRel} / ${README_FILES.join(' / ')} / package-lock.json / Cargo.lock）`,
    );
    return;
  }

  if (args.check) {
    for (const line of mismatches) console.error(`版本不一致: ${line}`);
    for (const line of lockProblems) console.error(`版本不一致: ${line}`);
    if (lockProblems.some((line) => line.includes('Cargo.lock'))) {
      console.error('Cargo.lock 由 cargo 维护：先改 Cargo.toml，再执行 `cargo update -p <包名> --precise <版本>` 或 `cargo build` 后重试');
    }
    if (lockProblems.some((line) => line.includes('package-lock.json') || line.includes('optionalDependencies'))) {
      console.error('运行 `node scripts/sync-optional-deps.mjs` 重新对齐 package-lock.json 后重试');
    }
    if (mismatches.length > 0) {
      console.error('运行 `npm run sync-version` 自动对齐后重试');
    }
    process.exit(1);
  }

  // 写入模式：只重写那行 version，保留其余内容与行尾。
  //
  // 整个写入是事务式的：动手前确认 Cargo.lock 与**当前** Cargo.toml 一致
  // （否则分不清是新旧版本差异还是历史漂移），写入后让 cargo 刷新 lock 并复核；
  // 任何一步失败都把已改文件按原内容写回，绝不留下「Cargo.toml 已升、Cargo.lock 没升」
  // 的半成品——那种状态会让下游 `cargo build --locked` 直接失败，而且很难看出原因。
  const cargoLockPath = join(dirname(cargoPath), 'Cargo.lock');
  const hasCargoLock = existsSync(cargoLockPath);
  if (hasCargoLock) {
    const drift = checkCargoLock(cargoPath, cargoName, cargoVersion, cargoRel);
    if (drift.length > 0) {
      throw new Error(
        `写入前检查失败，Cargo.lock 与当前 ${cargoRel} 不一致：${drift.join('；')}\n` +
          '先在一个干净的版本上把 lock 修好（`cargo update -p <包名> --precise <版本>`），再执行版本同步',
      );
    }
  }

  const backups = new Map();
  const writeTracked = (path, next) => {
    if (!backups.has(path)) backups.set(path, readFileSync(path, 'utf8'));
    writeFileSync(path, next);
  };
  const rollback = () => {
    const restored = [];
    for (const [path, text] of backups) {
      try {
        writeFileSync(path, text);
        restored.push(path);
      } catch {
        // 回滚失败也要继续尝试其余文件：能救一个是一个。
      }
    }
    return restored;
  };

  if (cargoVersion !== packageVersion) {
    const indent = lines[lineIndex].match(/^\s*/)[0];
    lines[lineIndex] = `${indent}version = "${packageVersion}"`;
    writeTracked(cargoPath, lines.join('\n'));
    console.log(`已把 ${cargoRel} 的版本从 ${cargoVersion} 改为 ${packageVersion}`);
  }
  for (const { file, next, found } of readmeChanges) {
    writeTracked(join(root, file), next);
    console.log(`已把 ${file} 的版本从 ${found.join(' / ')} 改为 ${packageVersion}`);
  }

  // Cargo.toml 改过之后 lock 必然落后一步：交给 cargo 刷新，再复核一遍。
  if (hasCargoLock && checkCargoLock(cargoPath, cargoName, packageVersion).length > 0) {
    const failure = refreshCargoLock(cargoPath, cargoName, packageVersion);
    const remaining = failure ? null : checkCargoLock(cargoPath, cargoName, packageVersion);
    if (failure !== null || remaining.length > 0) {
      const restored = rollback();
      throw new Error(
        `Cargo.lock 刷新失败${failure ? `：${failure}` : `：${remaining.join('；')}`}\n` +
          `已回滚 ${restored.length ? restored.join(' / ') : '(无文件需要回滚)'}；` +
          `请手动执行 \`cargo update -p ${cargoName} --precise ${packageVersion}\` 后重试`,
      );
    }
    console.log(`已用 cargo 把 Cargo.lock 中 ${cargoName} 的版本更新为 ${packageVersion}`);
  }

  const written = [];
  if (cargoVersion !== packageVersion) written.push(cargoRel);
  written.push(...readmeChanges.map(({ file }) => file));
  // `--cargo ../elsewhere/Cargo.toml` 的 lock 在仓库外，git add 只会报错，跳过。
  if (hasCargoLock) {
    const lockRel = relative(root, cargoLockPath);
    if (!lockRel.startsWith('..')) written.push(lockRel);
  }
  if (written.length > 0) gitAdd(root, written);

  if (args.expect && args.expect !== packageVersion) {
    console.error(`注意: 期望版本 = ${args.expect} ≠ package.json = ${packageVersion}`);
  }

  // 复核用**写完之后**的状态：上面的 lockProblems 是写入前算的，Cargo.lock 已经
  // 被 cargo 修好，不能拿旧结论去 exit 1。
  const remainingProblems = [
    ...checkLock(root, packageVersion, pkg.optionalDependencies),
    ...checkCargoLock(cargoPath, cargoName, packageVersion),
  ];
  if (remainingProblems.length > 0) {
    for (const line of remainingProblems) console.error(`版本不一致: ${line}`);
    console.error(
      'lock 文件不由本脚本写入：package-lock.json 用 `node scripts/sync-optional-deps.mjs`，Cargo.lock 用 cargo（--locked 构建以它为准）',
    );
    process.exit(1);
  }
}

try {
  main();
} catch (error) {
  console.error(`版本同步失败: ${error.message}`);
  process.exit(1);
}
