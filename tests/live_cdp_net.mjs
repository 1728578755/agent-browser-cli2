// 真实浏览器验收：网络层拦截（net --engine cdp）。
//
// 与 live_xhr_wait.mjs 的分工：那边验页面级 hook（fetch/XHR 包装），
// 这边验 chrome.debugger + Fetch 域——它能改 blob 的原始体，也能拦
// <img> / <script> 这类根本不经过 fetch/XHR 的请求。
//
// 前置：daemon 运行中，且有一个能 attach debugger 的浏览器目标
//      （页面里嵌了别的扩展 iframe 时 Chrome 会拒绝 attach，见 crbug 40753571）。
// 运行：node tests/live_cdp_net.mjs [--tab <tabId>] [--keep]
import http from "node:http";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CLI = process.env.ABC_CLI || path.join(ROOT, "target", "release", "agent-browser-cli.exe");
const KEEP = process.argv.includes("--keep");
const argOf = (name) => {
  const at = process.argv.indexOf(name);
  return at >= 0 ? process.argv[at + 1] : null;
};
const wantTab = argOf("--tab");
const wantBrowser = argOf("--browser");

const PAGE = `<!doctype html><html><head><meta charset="utf-8"><title>cdp net</title></head>
<body>
<img id="img" src="/pixel.png?t=__STAMP__" alt="px">
<script src="/sp.js"></script>
<script>
  window.runXhrBlob = function (url) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      xhr.open('GET', url);
      xhr.responseType = 'blob';
      xhr.onload = function () {
        xhr.response.arrayBuffer().then(function (buf) {
          var bytes = Array.from(new Uint8Array(buf));
          var text = '';
          try { text = new TextDecoder('utf-8').decode(new Uint8Array(buf)); } catch (e) {}
          resolve({ status: xhr.status, bytes: bytes, text: text, type: xhr.response.type });
        }, reject);
      };
      xhr.onerror = function () { reject(new Error('xhr error')); };
      xhr.send();
    });
  };
</script>
</body></html>`;

// 1x1 透明 PNG
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  "base64"
);

let passed = 0;
let failed = 0;
function assert(cond, name, detail) {
  if (cond) {
    passed += 1;
    console.log(`  PASS  ${name}`);
  } else {
    failed += 1;
    console.log(`  FAIL  ${name}`);
    if (detail !== undefined) console.log("        " + JSON.stringify(detail).slice(0, 600));
  }
}

function run(args, timeoutMs = 90000) {
  return new Promise((resolve) => {
    const child = spawn(CLI, args, { cwd: ROOT, windowsHide: true });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      try { child.kill(); } catch {}
      resolve({ code: -1, stdout, stderr: stderr + "\n[timeout]", json: null, elapsed: timeoutMs });
    }, timeoutMs);
    const started = Date.now();
    child.stdout.on("data", (chunk) => (stdout += chunk.toString("utf8")));
    child.stderr.on("data", (chunk) => (stderr += chunk.toString("utf8")));
    child.on("close", (code) => {
      clearTimeout(timer);
      let json = null;
      try { json = JSON.parse(stdout); } catch {}
      resolve({ code, stdout, stderr, json, elapsed: Date.now() - started });
    });
  });
}

function serve() {
  const server = http.createServer((req, res) => {
    const url = String(req.url || "");
    if (url === "/" || url.startsWith("/?")) {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      // 每次返回的 img URL 都不同，避免内存缓存命中导致请求根本不发出去。
      res.end(PAGE.replace("__STAMP__", String(Date.now())));
      return;
    }
    if (url.startsWith("/sp.js")) {
      res.writeHead(200, { "content-type": "text/javascript; charset=utf-8", "cache-control": "no-store" });
      res.end("window.__sp = 1;");
      return;
    }
    if (url.startsWith("/pixel.png")) {
      res.writeHead(200, { "content-type": "image/png", "content-length": String(PNG.length), "cache-control": "no-store" });
      res.end(PNG);
      return;
    }
    if (url.startsWith("/bin")) {
      const body = Buffer.from([0, 1, 2, 3]);
      res.writeHead(200, { "content-type": "application/octet-stream", "content-length": String(body.length), "cache-control": "no-store" });
      res.end(body);
      return;
    }
    res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" });
    res.end(JSON.stringify({ v: 1 }));
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve({ server, port: server.address().port }));
  });
}

const jsReturn = (result) => result.json?.result?.js_return ?? result.json?.js_return ?? null;

async function main() {
  const { server, port } = await serve();
  const url = `http://127.0.0.1:${port}/`;
  let tabId = wantTab;
  console.log(`本地服务: ${url}`);

  try {
    let health = await run(["status"]);
    if (!health.json?.ready) {
      await run(["tabs"]);
      await new Promise((r) => setTimeout(r, 2500));
      health = await run(["status"]);
    }
    if (!health.json?.ready) {
      console.log("daemon 未就绪，跳过：", health.stdout.slice(0, 160));
      process.exitCode = 2;
      return;
    }

    if (!tabId) {
      const openArgs = ["open", url, "--timeout", "25"];
      if (wantBrowser) openArgs.push("--browser", wantBrowser);
      const opened = await run(openArgs);
      tabId = opened.json?.result?.opened_tab_id || opened.json?.result?.metadata?.raw?.id;
      assert(!!tabId, "打开测试页", opened.json?.result);
    } else {
      await run(["open", url, "--tab", String(tabId), "--timeout", "25"]);
    }
    const tab = ["--tab", String(tabId)];

    // 先确认这个标签能 attach：嵌了别的扩展 iframe 的页面会被 Chrome 拒绝。
    const probe = await run(["network", "start", ...tab]);
    if (probe.json?.ok === false) {
      console.log("该标签无法 attach debugger，跳过 CDP 验收：");
      console.log("  " + String(probe.json?.error || probe.stdout).slice(0, 240));
      process.exitCode = 2;
      return;
    }
    await run(["network", "stop", ...tab]);

    const readState = async () => {
      const res = await run([
        "exec",
        "const img = document.getElementById('img'); return { sp: window.__sp, naturalWidth: img ? img.naturalWidth : null, imgError: !!img && img.complete && img.naturalWidth === 0 };",
        ...tab
      ]);
      return jsReturn(res);
    };

    console.log("1) 基线：脚本执行、图片加载");
    await run(["reload", ...tab]);
    await run(["wait", "--js", "window.__sp !== undefined && document.getElementById('img').complete", "--timeout", "15", ...tab]);
    let state = await readState();
    assert(state?.sp === 1, "基线 __sp=1", state);
    assert(state?.naturalWidth === 1, "基线图片已加载", state);

    console.log("2a) 对照：页面引擎（默认 --engine page）拦不到 <script>");
    await run(["net", "mock", "/sp.js", "--response-body", "window.__sp = 3;", ...tab]);
    await run(["reload", ...tab]);
    await run(["wait", "--js", "window.__sp !== undefined", "--timeout", "15", ...tab]);
    state = await readState();
    assert(state?.sp === 1, "页面引擎对 <script> 无效（仍为 1）", state);
    await run(["net", "reset", ...tab]);

    console.log("2) 拦截 <script> 并改写响应体（页面引擎做不到）");
    const scriptRule = await run(["net", "mock", "/sp.js", "--engine", "cdp", "--response-body", "window.__sp = 2;", ...tab]);
    assert(scriptRule.json?.result?.net?.engine === "cdp", "规则走 cdp 引擎", scriptRule.json?.result?.net);
    await run(["reload", ...tab]);
    await run(["wait", "--js", "window.__sp === 2", "--timeout", "15", ...tab]);
    state = await readState();
    assert(state?.sp === 2, "脚本被改成 __sp=2", state);

    console.log("3) 拦截 <img> 并替换图片字节（页面引擎做不到）");
    await run(["net", "mock", "/pixel.png", "--engine", "cdp", "--response-body", "not-a-png", ...tab]);
    await run(["reload", ...tab]);
    await run([
      "wait",
      "--js",
      "const img = document.getElementById('img'); return !!img && img.complete && img.naturalWidth === 0;",
      "--timeout", "15", ...tab
    ]);
    state = await readState();
    assert(state?.naturalWidth === 0, "图片字节被替换成非法内容（naturalWidth=0）", state);

    console.log("4) blob 响应只改状态码：页面引擎无法改写，CDP 可以");
    await run(["net", "mock", "/bin", "--engine", "cdp", "--response-status", "202", ...tab]);
    let blobRes = await run(["exec", "return await window.runXhrBlob('/bin')", ...tab]);
    const blobStatus = jsReturn(blobRes);
    assert(blobStatus?.status === 202, "blob 读到改写后的 202", blobStatus);
    assert(
      Array.isArray(blobStatus?.bytes) && blobStatus.bytes.length === 4 && blobStatus.bytes[0] === 0,
      "原始二进制体保持不变",
      blobStatus?.bytes
    );

    console.log("5) blob 响应改 body（二进制 → 文本）");
    await run(["net", "mock", "/bin", "--engine", "cdp", "--response-body", '{"x":1}', ...tab]);
    blobRes = await run(["exec", "return await window.runXhrBlob('/bin')", ...tab]);
    const blobBody = jsReturn(blobRes);
    assert(blobBody?.text === '{"x":1}', "blob 拿到改写后的文本", blobBody);

    console.log("6) routes 里能看到网络层规则");
    const routes = await run(["net", "routes", ...tab]);
    const cdp = routes.json?.result?.net?.cdp;
    assert(cdp?.enabled === true && cdp?.count >= 3, "cdp 规则数正确", cdp);
    assert(Array.isArray(cdp?.rules) && cdp.rules.some((r) => r.match === "/sp.js"), "规则内容可见", cdp?.rules);

    console.log("6b) net log 合并 CDP 侧记录");
    await run(["net", "hook", ...tab]);
    await run(["exec", "await fetch('/bin'); return 1", ...tab]);
    const merged = await run(["net", "log", "--limit", "50", ...tab]);
    const logPayload = merged.json?.result?.net;
    const logItems = logPayload?.items || [];
    assert(logItems.some((item) => item.source === "cdp"), "日志里有 CDP 侧记录", logItems.slice(-4));
    assert(logItems.some((item) => item.source === "page"), "日志里有页面侧记录", logItems.slice(-4));
    assert((logPayload?.sources?.cdp ?? 0) >= 1 && (logPayload?.sources?.page ?? 0) >= 1, "两边条数都统计到", logPayload?.sources);

    console.log("6c) --clear 同时清空两边");
    await run(["net", "log", "--clear", ...tab]);
    const cleared = await run(["net", "log", "--limit", "20", ...tab]);
    const clearedPayload = cleared.json?.result?.net;
    assert(
      (clearedPayload?.sources?.cdp ?? 0) === 0 && (clearedPayload?.sources?.page ?? 0) === 0,
      "清空后两边都是 0",
      clearedPayload?.sources
    );

    console.log("7) reset 后全部恢复");
    await run(["net", "reset", ...tab]);
    await run(["reload", ...tab]);
    await run(["wait", "--js", "window.__sp !== undefined && document.getElementById('img').complete", "--timeout", "15", ...tab]);
    state = await readState();
    assert(state?.sp === 1, "脚本恢复原始内容", state);
    assert(state?.naturalWidth === 1, "图片恢复加载", state);
    const afterReset = await run(["net", "routes", ...tab]);
    assert((afterReset.json?.result?.net?.cdp?.count ?? -1) === 0, "reset 清空 cdp 规则", afterReset.json?.result?.net?.cdp);
  } finally {
    if (tabId && !KEEP && !wantTab) await run(["close", "--tab", String(tabId)]);
    server.close();
  }

  console.log(`\n通过 ${passed}，失败 ${failed}`);
  if (failed > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
