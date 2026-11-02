// 校验扩展生成的页面注入脚本能被解析、且返回值正确。
//
// 这段模板字符串在 Node 里看不出语法错误（它是字符串），只有真正 eval 才会炸。
// 之前就因此留下过重复的 `let r;`，导致 exec 永远返回 null。用这个脚本兜底。
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = fs.readFileSync(path.join(here, "..", "assets", "tmwd_cdp_bridge", "background.js"), "utf8");

function extract(name, nextName) {
  const start = source.indexOf(`function ${name}`);
  if (start < 0) throw new Error(`找不到 ${name}`);
  const end = source.indexOf(`function ${nextName}`, start);
  if (end < 0) throw new Error(`找不到 ${nextName}`);
  return source.slice(start, end);
}

const snippet = [extract("buildExecScript", "buildPageScript"), extract("buildPageScript", "buildCdpScript")].join("\n");
const factory = new Function(`${snippet}\nreturn { buildExecScript, buildPageScript };`);
const { buildPageScript } = factory();

const cases = [
  { code: "return 1 + 1", expect: 2, label: "语句级 return" },
  { code: "1 + 1", expect: 2, label: "裸表达式" },
  { code: "(async () => { return 7 })()", expect: 7, label: "daemon 包装后的自执行函数" },
  { code: "await Promise.resolve(9)", expect: 9, label: "顶层 await" },
  { code: "const a = 1; a * 5", expect: 5, label: "多语句表达式" },
];

let failed = 0;
for (const item of cases) {
  const script = buildPageScript(item.code);
  let actual;
  let error = null;
  try {
    // 用同样是「字符串求值」的方式跑，模拟页面里的 eval。
    actual = await new Function(`return (${script})`)();
    if (actual && typeof actual === "object" && "ok" in actual) {
      if (!actual.ok) error = actual.error?.message || "script returned ok:false";
      actual = actual.data;
    }
  } catch (e) {
    error = e.message;
  }
  const ok = !error && JSON.stringify(actual) === JSON.stringify(item.expect);
  if (!ok) failed += 1;
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${item.label}  expect=${JSON.stringify(item.expect)} got=${JSON.stringify(actual)}${error ? " err=" + error : ""}`);
}

if (failed) {
  console.error(`page script builder FAILED (${failed})`);
  process.exit(1);
}

// 新增的页面脚本（read / find / action）只做「能解析 + 入口函数存在」的最小校验。
// 它们依赖 DOM，无法在 Node 里完整执行；但模板串里的语法错误能在这里提前发现。
const assetChecks = [
  { file: "page_read.js", entry: "__abcRead" },
  { file: "page_find.js", entries: ["__abcFind", "__abcCssPath", "__abcVisible"] },
  { file: "page_action.js", entries: ["__abcAct", "__abcResolve", "__abcDispatch"] },
  { file: "page_snapshot.js", entries: ["__abcSnapshot", "__abcSnapCssPath", "__abcSnapRole"] },
  { file: "page_net.js", entries: ["__abcNet"] },
  { file: "page_wait.js", entries: ["__abcWait"] },
  { file: "tmwd_cdp_bridge/persist_boot.js", entries: [] },
];
for (const check of assetChecks) {
  const source = fs.readFileSync(path.join(here, "..", "assets", check.file), "utf8");
  const entries = check.entries || [check.entry];
  try {
    new Function(source);
    const missing = entries.filter((name) => !source.includes(`function ${name}(`));
    if (missing.length) throw new Error(`缺少函数: ${missing.join(", ")}`);
    console.log(`  PASS  ${check.file} 可解析且入口齐全`);
  } catch (e) {
    failed += 1;
    console.error(`  FAIL  ${check.file}: ${e.message}`);
  }
}

if (failed) {
  console.error(`page script assets FAILED (${failed})`);
  process.exit(1);
}
console.log("page script builder OK");
