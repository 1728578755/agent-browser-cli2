#!/usr/bin/env node
// 页面脚本同步：以 assets/page_*.js 为唯一真相源，逐字节复制到扩展目录。
//
// 为什么需要这个脚本：
//   同一份页面脚本存在两个副本——
//     1. assets/page_*.js                    daemon 用 include_str! 打包，走 eval 通道；
//     2. assets/tmwd_cdp_bridge/ops/page_*.js 严格 CSP 页面下由扩展 chrome.scripting 注入。
//   两份手写维护必然会漂移（曾经 ops 侧落后 55 行，导致 CSP 页面上 challenge solve 假通过）。
//   所以这里只允许改主副本，ops 侧一律由本脚本生成。
//
// 用法：
//   node scripts/sync-extension-assets.mjs           # 把主副本覆盖到 ops/
//   node scripts/sync-extension-assets.mjs --check   # 只比对不写盘，有不一致则退出码 1（CI 用）
//
// 退出码：0 一致或已同步；1 --check 下存在不一致 / 文件缺失。

import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const PREFIX = 'page_';
const SUFFIX = '.js';

function parseArgs(argv) {
  const args = { check: false };
  for (const current of argv) {
    if (current === '--check') args.check = true;
    else if (current === '--help' || current === '-h') {
      console.log('用法: sync-extension-assets.mjs [--check]');
      process.exit(0);
    } else throw new Error(`未知参数: ${current}`);
  }
  return args;
}

function resolveRoot() {
  return resolve(dirname(fileURLToPath(import.meta.url)), '..');
}

function listPageScripts(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.startsWith(PREFIX) && name.endsWith(SUFFIX))
    .sort();
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const root = resolveRoot();
  const sourceDir = join(root, 'assets');
  const targetDir = join(root, 'assets', 'tmwd_cdp_bridge', 'ops');

  const names = listPageScripts(sourceDir);
  if (names.length === 0) throw new Error(`在 ${join('assets')} 下找不到 ${PREFIX}*${SUFFIX}`);

  let changed = 0;
  let same = 0;
  let missing = 0;
  const diffs = [];

  for (const name of names) {
    const from = join(sourceDir, name);
    const to = join(targetDir, name);
    const source = readFileSync(from);

    if (!existsSync(to)) {
      missing += 1;
      if (args.check) {
        diffs.push(`${name}  缺少 ops 副本（主副本 ${source.length} 字节）`);
      } else {
        mkdirSync(targetDir, { recursive: true });
        writeFileSync(to, source);
        changed += 1;
        console.log(`已同步    ${name}  (新建，${source.length} 字节)`);
      }
      continue;
    }

    const target = readFileSync(to);
    if (source.equals(target)) {
      same += 1;
      console.log(`无需变更  ${name}  (${source.length} 字节)`);
      continue;
    }

    if (args.check) {
      diffs.push(`${name}  主副本 ${source.length} 字节 / ops ${target.length} 字节`);
    } else {
      writeFileSync(to, source);
      changed += 1;
      console.log(`已同步    ${name}  (${target.length} -> ${source.length} 字节)`);
    }
  }

  // ops 侧独有的 page_*.js：说明有人只改了扩展副本，等于又造了一份真相。
  const orphans = listPageScripts(targetDir).filter((name) => !names.includes(name));
  for (const name of orphans) {
    console.warn(`注意      ${name} 只存在于 ops/，主副本缺失（assets/page_*.js 才是真相源）`);
  }

  if (args.check) {
    if (diffs.length || orphans.length) {
      for (const line of diffs) console.error(`不一致    ${line}`);
      console.error(`共 ${diffs.length} 个文件不一致${orphans.length ? `、${orphans.length} 个 ops 独有文件` : ''}`);
      console.error('运行 `node scripts/sync-extension-assets.mjs` 从主副本重新生成 ops/ 后重试');
      process.exit(1);
    }
    console.log(`扩展脚本与主副本一致（${same} 个文件）`);
    return;
  }

  console.log(`完成：${changed} 个已同步，${same} 个无需变更${missing ? `，${missing} 个为新建` : ''}`);
}

try {
  main();
} catch (error) {
  console.error(`扩展脚本同步失败: ${error.message}`);
  process.exit(1);
}
