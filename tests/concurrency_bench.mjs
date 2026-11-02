// 并发/大规模基准：真实浏览器上开 N 个标签，对比「并行批量」与「串行」的吞吐。
//
// 用法：
//   node tests/concurrency_bench.mjs                 # 默认 8 标签 × 3 轮
//   node tests/concurrency_bench.mjs --tabs 12 --rounds 5
//   node tests/concurrency_bench.mjs --keep          # 保留标签页（默认测完关闭）
//
// 说明：本机若每条 CLI 都会冷启动 daemon，墙钟时间会被冷启动主导，
// 所以对比表同时给出「daemon 内部耗时」（batch/run 的 elapsed_ms）与墙钟。
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "..");
const candidates = [
  path.join(root, "target", "release", "agent-browser-cli.exe"),
  path.join(root, "target", "release", "agent-browser-cli"),
  path.join(root, "target", "debug", "agent-browser-cli.exe"),
  path.join(root, "target", "debug", "agent-browser-cli"),
];
const bin = candidates.find((p) => fs.existsSync(p));
if (!bin) {
  console.error("找不到可执行文件，请先 cargo build --release");
  process.exit(1);
}

const argv = process.argv.slice(2);
const argOf = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? Number(argv[i + 1]) : fallback;
};
const TABS = Math.max(1, Math.min(argOf("tabs", 8), 32));
const ROUNDS = Math.max(1, Math.min(argOf("rounds", 3), 20));
const KEEP = argv.includes("--keep");
const CONCURRENCY = Math.min(TABS, 32);

function run(args, timeout = 180000) {
  const started = Date.now();
  const result = spawnSync(bin, args, { encoding: "utf8", timeout });
  const wall = Date.now() - started;
  if (result.error) throw result.error;
  const stdout = (result.stdout || "").trim();
  let json = null;
  try {
    json = JSON.parse(stdout.slice(stdout.indexOf("{")));
  } catch {
    /* 非 JSON */
  }
  return { code: result.status, json, stdout, stderr: result.stderr || "", wall };
}

function writeSteps(name, steps) {
  const file = path.join(root, "target", name);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify({ steps }), "utf8");
  return file;
}

const closeAll = (tabIds) => {
  for (const tabId of tabIds) run(["close", "--tab", String(tabId)], 30000);
};

const openedTabs = [];
let failed = 0;
const record = (ok, label, detail) => {
  if (!ok) failed += 1;
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${label}${detail ? "  " + detail : ""}`);
};

(async () => {
  console.log(`bin: ${path.basename(bin)}  tabs=${TABS} rounds=${ROUNDS}`);

  const boot = run(["tabs"], 120000);
  console.log(`daemon 冷启动 + tabs: ${boot.wall}ms`);
  if (!boot.json?.ok) {
    console.error("daemon 不可用:", boot.stdout || boot.stderr);
    process.exit(1);
  }

  // 1) 并发打开
  const urls = Array.from({ length: TABS }, (_, i) => `https://example.com/?bench=${i}`);
  const openResult = run(["open", ...urls, "--background"], 240000);
  const openWall = openResult.wall;
  const tabs = (openResult.json?.tabs || []).map((t) => String(t.result?.opened_tab_id || "")).filter(Boolean);
  tabs.forEach((id) => openedTabs.push(id));
  console.log(`并发打开 ${TABS} 个标签: daemon=${openResult.json?.elapsed_ms}ms wall=${openWall}ms 成功=${tabs.length}`);
  record(tabs.length === TABS, "全部标签打开成功", `${tabs.length}/${TABS}`);

  if (tabs.length === 0) {
    console.log("没有可用标签，终止");
    process.exit(1);
  }

  // 2) 单标签基线（串行的一条命令）
  const single = run(["exec", "return location.search", "--tab", tabs[0]], 60000);
  const singleWall = single.wall;
  console.log(`单标签一次 exec（含 CLI 冷启动）: ${singleWall}ms`);

  // 2.5) 预热：每个标签各跑一次，避免把「后台标签解冻 / 首次注入」算进第一轮并发
  const warmSteps = tabs.map((tabId) => ({
    op: "exec",
    args: { script: "return 1", switch_tab_id: tabId },
  }));
  run(["batch", writeSteps("bench-warm.json", warmSteps), "--concurrency", String(CONCURRENCY)], 180000);

  // 3) 并行批量 vs 串行
  const steps = [];
  for (let round = 0; round < ROUNDS; round += 1) {
    for (let i = 0; i < tabs.length; i += 1) {
      // 用 read 作为负载：一次真实的结构化读取（遍历 + 序列化），
      // 单步耗时远高于噪声，才能真正看出并发收益。
      steps.push({
        op: round % 2 === 0 ? "read" : "exec",
        args: round % 2 === 0
          ? { max_chars: 4000, links: true, switch_tab_id: tabs[i] }
          : { script: "const t0=performance.now(); let n=0; while(performance.now()-t0<15){n++;} return n", switch_tab_id: tabs[i] },
      });
    }
  }
  const stepFile = writeSteps("bench-steps.json", steps);

  // 先量两轮并行取较小值（一轮可能赶上 GC / 调度抖动），再量串行。
  const parallelA = run(["batch", stepFile, "--concurrency", String(CONCURRENCY)], 600000);
  const parallelB = run(["batch", stepFile, "--concurrency", String(CONCURRENCY)], 600000);
  const serial = run(["run", stepFile], 600000);

  const parallelElapsed = Math.min(
    parallelA.json?.elapsed_ms ?? Number.MAX_SAFE_INTEGER,
    parallelB.json?.elapsed_ms ?? Number.MAX_SAFE_INTEGER,
  );
  const parallel = parallelElapsed === (parallelA.json?.elapsed_ms ?? -1) ? parallelA : parallelB;
  const serialElapsed = (serial.json?.steps || []).reduce((sum, s) => sum + (s.elapsed_ms || 0), 0);
  const speedup = serialElapsed > 0 && parallelElapsed > 0 && Number.isFinite(parallelElapsed)
    ? (serialElapsed / parallelElapsed).toFixed(2)
    : "n/a";
  console.log(`\n批量 ${steps.length} 步（${TABS} 标签 × ${ROUNDS} 轮）`);
  console.log(`  并行 batch(concurrency=${CONCURRENCY}): daemon=${parallelElapsed}ms（两轮：${parallelA.json?.elapsed_ms}/${parallelB.json?.elapsed_ms}）wall=${parallel.wall}ms failed=${parallel.json?.failed}`);
  console.log(`  串行 run:                            各步合计=${serialElapsed}ms wall=${serial.wall}ms failed=${(serial.json?.steps || []).filter((s) => !s.ok).length}`);
  console.log(`  加速比（daemon 内部）: ${speedup}x`);

  record(parallel.json?.ok === true, "batch 全部成功", `count=${parallel.json?.count}`);
  record(serial.json?.ok === true, "run 全部成功", `count=${serial.json?.count}`);

  // 4) 并发隔离：每个标签必须返回自己的 query
  const isoSteps = tabs.map((tabId, i) => ({
    op: "exec",
    args: { script: "return location.search", switch_tab_id: tabId },
  }));
  const isoFile = writeSteps("bench-isolation.json", isoSteps);
  const iso = run(["batch", isoFile, "--concurrency", String(CONCURRENCY)], 180000);
  const results = (iso.json?.steps || []).map((s) => (s.result?.js_return || "").trim());
  const isolated = results.length === tabs.length && results.every((value, i) => value === `?bench=${i}`);
  record(isolated, "并发隔离：每个标签返回自己的 URL", isolated ? "" : JSON.stringify(results));

  // 5) 每标签吞吐（同一标签并发多条命令，验证不会串行化到不可用）
  const hotSteps = Array.from({ length: 12 }, () => ({
    op: "exec",
    args: { script: "return 1", switch_tab_id: tabs[0] },
  }));
  const hotFile = writeSteps("bench-hot.json", hotSteps);
  const hot = run(["batch", hotFile, "--concurrency", "12"], 180000);
  console.log(`同标签 12 条并发命令: daemon=${hot.json?.elapsed_ms}ms failed=${hot.json?.failed}`);
  record(hot.json?.ok === true, "同标签并发命令全部成功", `${hot.json?.elapsed_ms}ms`);

  if (!KEEP) closeAll(openedTabs);
  console.log(`\n${failed ? "BENCH FAILED" : "BENCH PASSED"}`);
  process.exit(failed ? 1 : 0);
})().catch((err) => {
  console.error(err);
  if (!KEEP) closeAll(openedTabs);
  process.exit(1);
});
