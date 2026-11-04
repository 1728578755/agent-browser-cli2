#!/usr/bin/env node
// 把仓库里的扩展（assets/atb_bridge）部署到浏览器实际加载的目录。
//
// 为什么要专门写一个脚本，而不是 cp -r：
//   1. Chrome 的「重新加载」和 daemon 的自动重载都是**异步**的。如果那一刻
//      background.js 正被覆盖写入一半，Service Worker 会读到残缺脚本，
//      直接报 `Service worker registration failed. Status code: 2`，
//      而且这条错误会一直挂在 chrome://extensions 上，看起来像代码坏了。
//      所以每个文件都「先写临时文件、再 rename」——同目录 rename 在 Windows 上
//      走 MoveFileEx(MOVEFILE_REPLACE_EXISTING)，是原子替换，不会出现半截文件。
//   2. 部署完必须逐字节校验：拷错目录 / 拷漏 ops/ 是这套工具最常见的事故。
//
// 用法：
//   node scripts/deploy-extension.mjs --to "E:\some\dir"
//   ATB_EXT_DIR="E:\some\dir" node scripts/deploy-extension.mjs
//   node scripts/deploy-extension.mjs --dry-run
//
// 目标目录没有默认值：写死本机路径只对作者有效，换台机器就会在错误位置
// 静默建出一套扩展（v6 P3）。必须用 --to 或 ATB_EXT_DIR 显式指定。
import { createHash } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  rmdirSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/// 部署记录：目标目录里上一次由本脚本写过的文件列表。
/// 用它做「源里删掉的文件同步删掉」，同时避免乱删用户在目标目录里放的其他东西。
///
/// 写入走 `<记录>.tmp` + rename；崩溃恢复时只认最终文件名，`.tmp` 一律当残留清掉，
/// 绝不把半截临时文件当成有效记录。
const DEPLOY_RECORD = '.atb-deployed.json';

function parseArgs(argv) {
  const args = { target: undefined, dryRun: false, check: false };
  for (let i = 0; i < argv.length; i += 1) {
    const current = argv[i];
    if (current === '--to') args.target = argv[++i];
    else if (current === '--dry-run') args.dryRun = true;
    // `--check` 是 `--dry-run` 的语义别名（CI 里读起来更像「只检查不写」）。
    else if (current === '--check') args.check = true;
    else if (current === '--help' || current === '-h') {
      console.log('用法: node scripts/deploy-extension.mjs --to <目录> [--dry-run|--check]');
      console.log('  或用环境变量 ATB_EXT_DIR 指定目标目录。');
      process.exit(0);
    } else throw new Error(`未知参数: ${current}`);
  }
  return args;
}

function listFiles(dir, prefix = '') {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    // 备份文件（background.js.bak-*）不进部署目录，避免和真实入口混淆。
    if (/\.bak(\.|$)/.test(entry.name)) continue;
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...listFiles(join(dir, entry.name), rel));
    else out.push(rel);
  }
  return out.sort();
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

/// 原子写入：同目录临时文件 + rename。
///
/// rename 失败（Windows 上目标被浏览器占用、跨设备、权限不足）时必须把临时文件删掉：
/// 否则崩溃/失败的每一次部署都会在扩展目录里堆一个 `.atb-tmp-*`，下次
/// `listFiles`/人工排查都会被它们干扰。
function atomicCopy(from, to) {
  mkdirSync(dirname(to), { recursive: true });
  const tmp = join(dirname(to), `.atb-tmp-${process.pid}-${Date.now()}-${to.split(/[\\/]/).pop()}`);
  try {
    copyFileSync(from, tmp);
    renameSync(tmp, to);
  } catch (error) {
    try {
      rmSync(tmp, { force: true });
    } catch {
      // 清理失败不能盖掉真正的错误原因，忽略即可。
    }
    throw error;
  }
}

/// 清理上一次部署崩溃留下的临时文件。
///
/// 只删两类自己生成的名字（`.atb-tmp-*` 与部署记录的 `.tmp`），绝不碰目标目录里
/// 用户放的其他文件。
function cleanupTemp(target) {
  let removed = 0;
  const pending = [target];
  while (pending.length > 0) {
    const dir = pending.pop();
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        pending.push(full);
      } else if (entry.name.startsWith('.atb-tmp-') || entry.name === `${DEPLOY_RECORD}.tmp`) {
        try {
          rmSync(full, { force: true });
          removed += 1;
        } catch {
          // 被占用就留到下次，不影响本次部署。
        }
      }
    }
  }
  return removed;
}

/// 部署记录的原子写入（先写 `.tmp` 再 rename）。
///
/// 记录文件是「下次删哪些文件」的唯一依据：直接 `writeFileSync` 覆盖，进程恰好死在
/// 写一半时文件就是半截 JSON，`readRecord` 只能当成首次部署 → 旧文件永远清不掉。
function writeRecord(target, record) {
  const recordPath = join(target, DEPLOY_RECORD);
  const tmp = `${recordPath}.tmp`;
  try {
    writeFileSync(tmp, record, 'utf8');
    renameSync(tmp, recordPath);
  } catch (error) {
    try {
      rmSync(tmp, { force: true });
    } catch {
      // 同上：不覆盖真正的失败原因。
    }
    throw error;
  }
}

/// 只接受目标目录内的相对路径，防止记录文件被篡改成 `../../x` 后误删。
function isSafeRelative(rel) {
  if (typeof rel !== 'string' || rel.length === 0) return false;
  if (/^[A-Za-z]:/.test(rel) || rel.startsWith('/') || rel.startsWith('\\')) return false;
  return !rel.split(/[\\/]/).some((part) => part === '..');
}

function readRecord(target) {
  const recordPath = join(target, DEPLOY_RECORD);
  if (!existsSync(recordPath)) return null;
  try {
    const data = JSON.parse(readFileSync(recordPath, 'utf8'));
    if (!Array.isArray(data.files)) return null;
    return data.files.filter(isSafeRelative);
  } catch {
    // 记录损坏时按「首次部署」处理：宁可少删，不可乱删。
    return null;
  }
}

/// 删除目标目录里「上次部署过、这次源里已经没有」的文件，并清理空目录。
function removeStale(target, stale, dryRun) {
  const removed = [];
  for (const rel of stale) {
    const full = join(target, rel);
    if (!existsSync(full)) continue;
    if (dryRun) {
      console.log(`  计划删除  ${rel}`);
      continue;
    }
    rmSync(full, { force: true });
    removed.push(rel);
    // 自底向上删空目录；失败（目录非空/被占用）就保留，不算错误。
    let dir = dirname(rel);
    while (dir && dir !== '.') {
      try {
        rmdirSync(join(target, dir));
      } catch {
        break;
      }
      dir = dirname(dir);
    }
  }
  return removed;
}

/// 没有部署记录时的兼容清理：ops/ 下已经不在源里的 page_*.js 一定是旧逻辑。
function legacyStale(target, files) {
  const opsDir = join(target, 'ops');
  if (!existsSync(opsDir)) return [];
  return readdirSync(opsDir)
    .filter((name) => name.startsWith('page_') && name.endsWith('.js') && !files.includes(`ops/${name}`))
    .map((name) => `ops/${name}`);
}

/// 清理历史备份：`background.js.bak-20261101` 这类手工/旧脚本留下的快照。
///
/// 只认「我们这次确实部署过的文件名 + `.bak-` + 8 位以上纯数字」这一种形态：
///   - 用户自己的 `background.js.bak`（不带时间戳）不动；
///   - 目标目录里别的文件（哪怕也带 `.bak-日期`）不动；
///   - `.atb-deployed.json`（部署记录）永远不在删除范围内。
/// 目录按「部署文件的相对目录」逐个扫描，子目录（ops/ 等）里的备份同样能清掉。
function pruneBackups(target, files, dryRun) {
  const byDir = new Map();
  for (const rel of files) {
    const name = rel.split(/[\\/]/).pop();
    if (!name || name === DEPLOY_RECORD) continue;
    if (/\.bak(\.|$)/.test(name)) continue;
    const dir = dirname(rel);
    if (!byDir.has(dir)) byDir.set(dir, new Set());
    byDir.get(dir).add(name);
  }
  const pruned = [];
  for (const [dir, names] of byDir) {
    const abs = dir === '.' ? target : join(target, dir);
    let entries;
    try {
      entries = readdirSync(abs, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isFile()) continue;
      for (const name of names) {
        const prefix = `${name}.bak-`;
        if (!entry.name.startsWith(prefix)) continue;
        // 时间戳段必须是 8 位以上纯数字（YYYYMMDD 起），允许末尾一个字母后缀
        // （旧版部署脚本会生成 `.bak-20261101b/c/d` 这种同日多次备份），
        // 别把 `.bak-old` / `.bak-1` 当垃圾删。
        if (!/^\d{8,}[a-z]?$/.test(entry.name.slice(prefix.length))) continue;
        const rel = dir === '.' ? entry.name : `${dir}/${entry.name}`;
        if (!dryRun) rmSync(join(target, rel), { force: true });
        pruned.push(rel);
        break;
      }
    }
  }
  pruned.sort();
  console.log(`pruned: ${pruned.length}`);
  for (const rel of pruned) {
    console.log(`  ${dryRun ? '计划删除备份' : '已删除备份'}  ${rel}`);
  }
  return pruned;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const source = join(root, 'assets', 'atb_bridge');
  const targetArg = args.target ?? process.env.ATB_EXT_DIR;
  if (!targetArg) {
    throw new Error(
      '未指定扩展目标目录。请用 `--to "<目录>"` 或环境变量 ATB_EXT_DIR 指定\n' +
        '  （目录必须是 chrome://extensions 里「加载已解压的扩展程序」选中的那个）。',
    );
  }
  const target = resolve(targetArg);
  // `--check` 与 `--dry-run` 等价：只报告会改什么，不落任何写操作。
  const checkOnly = args.dryRun || args.check;

  if (!existsSync(join(source, 'manifest.json'))) throw new Error(`源目录缺少 manifest.json: ${source}`);
  if (!existsSync(target)) throw new Error(`目标目录不存在: ${target}（先在 chrome://extensions 里加载一次）`);

  const manifest = JSON.parse(readFileSync(join(source, 'manifest.json'), 'utf8'));
  const files = listFiles(source);

  // 上一次部署崩在中途会在目标目录留下 `.atb-tmp-*` / 半截记录：先清掉再算差异，
  // 否则它们会被当成「用户文件」长期留着，也可能让下面的删除逻辑看错目录状态。
  if (!checkOnly) {
    const cleaned = cleanupTemp(target);
    if (cleaned) console.log(`清理    ${cleaned} 个上次残留的临时文件`);
  }

  // 上次部署记录 → 本次源里已不存在的文件。没有记录时退回只清理 ops/ 旧脚本。
  const previous = readRecord(target);
  const stale = previous
    ? previous.filter((rel) => !files.includes(rel))
    : legacyStale(target, files);

  console.log(`源      ${source}`);
  console.log(`目标    ${target}`);
  console.log(`版本    ${manifest.version}（${manifest.name}）`);
  console.log(`文件    ${files.length} 个`);
  if (stale.length) console.log(`待清理  ${stale.length} 个（源目录已删除）`);

  const copied = [];
  const failed = [];
  for (const rel of files) {
    const from = join(source, rel);
    const to = join(target, rel);
    if (checkOnly) {
      console.log(`  计划  ${rel}  (${statSync(from).size} 字节)`);
      continue;
    }
    try {
      atomicCopy(from, to);
      const ok = sha256(from) === sha256(to);
      if (ok) copied.push(rel);
      else failed.push(`${rel} 校验不一致`);
    } catch (error) {
      failed.push(`${rel}: ${error.message}`);
    }
  }

  if (checkOnly) {
    // check 只报告将删除什么，不碰目标目录。
    removeStale(target, stale, true);
    pruneBackups(target, files, true);
    console.log(`\n${args.check ? '--check' : '--dry-run'}：没有写任何文件`);
    return;
  }

  console.log(`\n已部署 ${copied.length}/${files.length} 个文件（逐字节校验通过）`);
  if (failed.length) {
    for (const item of failed) console.error(`  失败 ${item}`);
    process.exit(1);
  }

  const removed = removeStale(target, stale, false);
  if (removed.length) console.log(`已清理 ${removed.length} 个目标目录多余文件: ${removed.join(', ')}`);

  // 历史备份（`*.bak-YYYYMMDD…`）每次部署顺手清掉：扩展目录里堆十几份旧
  // background.js 时，chrome://extensions 的报错行号会对不上真正在跑的那份。
  pruneBackups(target, files, false);

  // 写部署记录：下次据此清理「源里删掉的文件」，同时不碰用户自己放进去的东西。
  // 必须原子写：半截 JSON 会让下一次部署退化成「首次部署」，旧文件永远清不掉。
  writeRecord(
    target,
    JSON.stringify({ generated_by: 'scripts/deploy-extension.mjs', version: manifest.version, files }, null, 2) +
      '\n',
  );
}

try {
  main();
} catch (error) {
  console.error(`部署失败: ${error.message}`);
  process.exit(1);
}
