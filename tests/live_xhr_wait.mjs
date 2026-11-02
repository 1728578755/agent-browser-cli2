// 真实浏览器验收：XHR 响应改写可靠性 + wait --js 单次注入。
//
// 与 mock_extension.mjs 的分工：
//   mock 桩验证协议链路（不跑页面 JS）；这个脚本连真实 Chrome/Edge，
//   验证「页面监听器比我们更早注册」时改写是否仍然生效，以及 wait --js
//   是否只注入一次。
//
// 前置：daemon 运行中，且浏览器装好 tmwd_cdp_bridge 扩展。
// 运行：node tests/live_xhr_wait.mjs [--keep]
import http from "node:http";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CLI = process.env.ABC_CLI || path.join(ROOT, "target", "release", "agent-browser-cli.exe");
const KEEP = process.argv.includes("--keep");

const PAGE = `<!doctype html><html><head><meta charset="utf-8"><title>xhr rewrite</title></head>
<body><div id="out">idle</div>
<script>
  // 关键：监听器全部在 send() 之前注册——这正是旧实现「改写不可靠」的场景。
  window.runXhr = function (url) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      var seen = { listeners_before_send: true };
      xhr.open('GET', url);
      xhr.onreadystatechange = function () {
        if (xhr.readyState === 2) seen.status_at_headers = xhr.status;
        if (xhr.readyState === 4) seen.status_at_state4 = xhr.status;
      };
      xhr.onload = function () {
        seen.status = xhr.status;
        seen.text = xhr.responseText;
        seen.header = xhr.getResponseHeader('x-mock');
        seen.origin_header = xhr.getResponseHeader('x-origin');
        seen.all = xhr.getAllResponseHeaders();
        resolve(seen);
      };
      xhr.onerror = function () { reject(new Error('xhr error')); };
      xhr.send();
    });
  };
  window.runXhrJson = function (url) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      xhr.open('GET', url);
      xhr.responseType = 'json';
      xhr.onload = function () {
        var text = null;
        try { text = xhr.responseText; } catch (e) { text = 'throws:' + e.name; }
        resolve({ status: xhr.status, json: xhr.response, response_text: text });
      };
      xhr.onerror = function () { reject(new Error('xhr error')); };
      xhr.send();
    });
  };
  window.runXhrBuffer = function (url) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      xhr.open('GET', url);
      xhr.responseType = 'arraybuffer';
      xhr.onload = function () {
        var text = null;
        try { text = new TextDecoder('utf-8').decode(new Uint8Array(xhr.response)); } catch (e) { text = 'decode-failed'; }
        resolve({ status: xhr.status, text: text, bytes: xhr.response ? xhr.response.byteLength : 0 });
      };
      xhr.onerror = function () { reject(new Error('xhr error')); };
      xhr.send();
    });
  };
  window.runXhrPost = function (url, body) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      xhr.open('POST', url);
      xhr.setRequestHeader('content-type', 'application/json');
      xhr.setRequestHeader('x-token', 'origin');
      xhr.onload = function () { resolve({ status: xhr.status, text: xhr.responseText }); };
      xhr.onerror = function () { reject(new Error('xhr error')); };
      xhr.send(JSON.stringify(body));
    });
  };
  window.runXhrAbort = function (url) {
    return new Promise(function (resolve) {
      var xhr = new XMLHttpRequest();
      var seen = { error: false, load: false, loadend: false };
      xhr.open('GET', url);
      xhr.onerror = function () { seen.error = true; };
      xhr.onload = function () { seen.load = true; };
      xhr.onloadend = function () { seen.loadend = true; setTimeout(function () { resolve(seen); }, 0); };
      xhr.send();
    });
  };
  window.runXhrBlob = function (url) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      xhr.open('GET', url);
      xhr.responseType = 'blob';
      xhr.onload = function () {
        var blob = xhr.response;
        blob.text().then(function (text) {
          resolve({ status: xhr.status, text: text, type: blob.type, size: blob.size });
        }, function (e) { reject(e); });
      };
      xhr.onerror = function () { reject(new Error('xhr error')); };
      xhr.send();
    });
  };
</script></body></html>`;

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
    if (req.url === "/" || req.url.startsWith("/?")) {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      res.end(PAGE);
      return;
    }
    if (req.url.startsWith("/echo")) {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" });
        res.end(JSON.stringify({ headers: req.headers, body }));
      });
      return;
    }
    if (req.url.startsWith("/csp")) {
      // 只禁 eval / new Function，不禁内联脚本：验证「页面 CSP 禁 eval」时
      // wait --js 仍能靠 CDP 回退完成，而且同样只注入一次。
      res.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      res.end(`<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="script-src 'self' 'unsafe-inline'">
<title>csp page</title></head>
<body><div id="mark">csp</div>
<script>
  window.__cspFlag = 0;
  setTimeout(function () { window.__cspFlag = 1; document.getElementById('mark').textContent = 'ready'; }, 600);
</script>
</body></html>`);
      return;
    }
    res.writeHead(200, {
      "content-type": "application/json",
      "x-origin": "1",
      "cache-control": "no-store"
    });
    res.end(JSON.stringify({ v: 1, name: "origin" }));
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve({ server, port: server.address().port }));
  });
}

const jsReturn = (result) => result.json?.result?.js_return ?? result.json?.js_return ?? null;

// daemon 把 --response-body 的 JSON 解析成对象再序列化，键顺序可能变，
// 所以比较「解析后的值」而不是原文。
function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b || a === null || b === null) return false;
  if (typeof a !== "object") return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  return ka.every((key) => deepEqual(a[key], b[key]));
}

function sameJson(text, expected) {
  try {
    return deepEqual(JSON.parse(text), expected);
  } catch {
    return false;
  }
}

async function main() {
  const { server, port } = await serve();
  const url = `http://127.0.0.1:${port}/`;
  let tabId = null;
  let extraTabId = null;
  let extraTabId2 = null;
  let extraTabId3 = null;
  console.log(`本地服务: ${url}`);

  try {
    let health = await run(["status"]);
    if (!health.json?.ready) {
      // status 不会拉起 daemon；tabs 会自动启动它。
      await run(["tabs"]);
      await new Promise((r) => setTimeout(r, 2500));
      health = await run(["status"]);
    }
    if (!health.json?.ready) {
      console.log("daemon 未就绪，跳过真实浏览器验收：", health.stdout.slice(0, 200));
      process.exitCode = 2;
      return;
    }

    console.log("1) 打开测试页");
    const opened = await run(["open", url, "--timeout", "25"]);
    tabId = opened.json?.result?.opened_tab_id || opened.json?.result?.tab_id;
    assert(!!tabId, "拿到 tab id", opened.json);
    const tab = ["--tab", String(tabId)];

    console.log("2) 基线：没有规则时读真实响应");
    await run(["net", "reset", ...tab]);
    let res = await run(["exec", "return await window.runXhr('/api')", ...tab]);
    let seen = jsReturn(res);
    assert(seen?.status === 200, "基线 status=200", seen);
    assert(String(seen?.text || "").includes('"v":1'), "基线是原始响应体", seen?.text);

    console.log("3) 放行真实请求 + 改写响应（监听器先于 send 注册）");
    const route = await run([
      "net", "mock", "/api",
      "--response-status", "201",
      "--response-body", '{"v":3,"name":"rewritten"}',
      "--header", "x-mock: yes",
      ...tab
    ]);
    assert(route.json?.result?.net?.ok === true, "规则安装成功", route.json?.result?.net);

    res = await run(["exec", "return await window.runXhr('/api')", ...tab]);
    seen = jsReturn(res);
    assert(seen?.status === 201, "onload 读到改写后的 status", seen);
    assert(seen?.status_at_state4 === 201, "readyState=4 的 readystatechange 也读到 201", seen);
    assert(sameJson(seen?.text, { v: 3, name: "rewritten" }), "onload 读到改写后的 responseText", seen?.text);
    assert(seen?.header === "yes", "getResponseHeader 读到改写后的头", seen?.header);
    assert(String(seen?.all || "").includes("x-mock: yes"), "getAllResponseHeaders 含改写后的头", seen?.all);
    assert(seen?.origin_header === "1", "未改写的原始响应头仍然保留", seen?.origin_header);

    console.log("4) responseType=json 的改写");
    res = await run(["exec", "return await window.runXhrJson('/api')", ...tab]);
    const asJson = jsReturn(res);
    assert(asJson?.status === 201, "json 模式 status 被改写", asJson);
    assert(asJson?.json?.v === 3 && asJson.json?.name === "rewritten", "json 模式 response 是改写后的对象", asJson);
    assert(String(asJson?.response_text || "").startsWith("throws:"), "responseType=json 时 responseText 仍按原生抛错", asJson?.response_text);

    console.log("5) responseType=arraybuffer 的改写");
    res = await run(["exec", "return await window.runXhrBuffer('/api')", ...tab]);
    const asBuffer = jsReturn(res);
    assert(sameJson(asBuffer?.text, { v: 3, name: "rewritten" }), "arraybuffer 解码后是改写后的内容", asBuffer);

    console.log("6) net log 记录了改写");
    const log = await run(["net", "log", "--limit", "20", ...tab]);
    const items = log.json?.result?.net?.items || [];
    const xhrItems = items.filter((item) => item.kind === "xhr");
    assert(xhrItems.some((item) => item.response_modified === true), "log 里出现 response_modified", xhrItems.slice(-3));
    assert(xhrItems.every((item) => item.status !== 0), "log 里的 status 不是 0", xhrItems.slice(-3));

    console.log("7) reset 后恢复真实响应");
    await run(["net", "reset", ...tab]);
    res = await run(["exec", "return await window.runXhr('/api')", ...tab]);
    seen = jsReturn(res);
    assert(seen?.status === 200 && String(seen?.text || "").includes('"v":1'), "reset 后回到原始响应", seen);

    console.log("8) wait --js：定时器条件，只注入一次");
    // 武装与等待是两个进程，wait 真正开始时条件可能已经过了一部分，
    // 所以留足余量（1.2s 后成立，断言至少等了 0.9s）。
    await run(["exec", "window.__flag = 0; setTimeout(function(){ window.__flag = 1; }, 1200); return 1", ...tab]);
    res = await run(["wait", "--js", "window.__flag === 1", "--timeout", "5", ...tab]);
    const wait1 = res.json?.result || res.json;
    assert(wait1?.matched === true, "定时器条件命中", wait1);
    assert((wait1?.injections ?? 99) === 1, "--js 只注入一次", wait1);
    assert((wait1?.elapsed_ms ?? 0) >= 900, "确实等到了条件成立", wait1);
    assert((wait1?.checks ?? 0) >= 1, "页面内求值次数被记录", wait1);

    console.log("9) wait --js：DOM 条件（MutationObserver 触发）");
    await run(["exec", "setTimeout(function(){ var d=document.createElement('div'); d.id='late'; d.textContent='ready'; document.body.appendChild(d); }, 500); return 1", ...tab]);
    res = await run(["wait", "--js", "return document.querySelector('#late')", "--timeout", "5", ...tab]);
    const wait2 = res.json?.result || res.json;
    assert(wait2?.matched === true, "DOM 条件命中", wait2);
    assert(wait2?.value?.tag === "div", "返回命中节点的描述", wait2?.value);
    assert((wait2?.injections ?? 99) === 1, "DOM 条件也只注入一次", wait2);

    console.log("10) wait --js 超时只注入一次");
    res = await run(["wait", "--js", "false", "--timeout", "0.8", ...tab]);
    const wait3 = res.json?.result || res.json;
    assert(wait3?.ok === false && wait3?.matched === false, "超时返回未命中", wait3);
    assert((wait3?.injections ?? 99) === 1, "超时也只注入一次", wait3);
    assert((wait3?.elapsed_ms ?? 0) >= 700, "等满超时才返回", wait3);

    console.log("11) wait --js 不再堵住同一标签的其他命令");
    await run(["exec", "window.__flag2 = 0; setTimeout(function(){ window.__flag2 = 1; }, 2000); return 1", ...tab]);
    const asyncWait = await run(["wait", "--js", "window.__flag2 === 1", "--timeout", "6", "--async", ...tab]);
    const jobId = asyncWait.json?.job?.id;
    assert(typeof jobId === "string", "--async 返回 job id", asyncWait.json);
    const during = await run(["read", "--max-chars", "200", ...tab]);
    assert(during.elapsed < 1500, "等待期间 read 不被阻塞", { elapsed_ms: during.elapsed });
    const done = await run(["job", "wait", jobId, "--timeout", "20"]);
    assert(done.json?.job?.result?.matched === true, "后台等待最终命中", done.json?.job?.result);

    console.log("12) exec --wait-js 同样是一次注入");
    res = await run(["exec", "window.__f3 = 0; setTimeout(function(){ window.__f3 = 1; }, 400); return 'ok'", "--wait-js", "window.__f3 === 1", "--wait-timeout", "5", ...tab]);
    const opWait = res.json?.result?.js_return?.wait;
    assert(opWait?.matched === true, "--wait-js 命中", opWait);
    assert((opWait?.injections ?? 99) === 1, "--wait-js 只注入一次", opWait);

    console.log("13) selector / text 等待未回归");
    res = await run(["wait", "--selector", "#late", "--timeout", "3", ...tab]);
    const sel = res.json?.result || res.json;
    assert(sel?.matched === true, "selector 命中", sel);
    res = await run(["wait", "--text", "ready", "--timeout", "3", ...tab]);
    const txt = res.json?.result || res.json;
    assert(txt?.matched === true, "text 命中", txt);

    console.log("14) 纯 mock（不发真实请求）");
    await run(["net", "mock", "/api", "--status", "503", "--body", '{"err":"boom"}', ...tab]);
    res = await run(["exec", "return await window.runXhr('/api')", ...tab]);
    const mocked = jsReturn(res);
    assert(mocked?.status === 503, "mock 的 status 生效", mocked);
    assert(sameJson(mocked?.text, { err: "boom" }), "mock 的 body 生效", mocked?.text);
    assert(
      String(mocked?.all || "").includes("content-type: application/json"),
      "mock 默认带 content-type",
      mocked?.all
    );

    console.log("15) abort 规则");
    await run(["net", "mock", "/api", "--abort", ...tab]);
    res = await run(["exec", "return await window.runXhrAbort('/api')", ...tab]);
    const aborted = jsReturn(res);
    assert(aborted?.error === true && aborted?.load === false, "trigger error 事件", aborted);
    assert(aborted?.loadend === true, "loadend 仍然触发", aborted);

    console.log("16) XHR 请求头 / 请求体改写");
    await run([
      "net", "mock", "/echo",
      "--request-header", "x-token: rewritten",
      "--request-body", '{"changed":true}',
      ...tab
    ]);
    res = await run(["exec", "return await window.runXhrPost('/echo', {v:1})", ...tab]);
    const echoed = jsReturn(res);
    let parsedEcho = null;
    try { parsedEcho = JSON.parse(echoed?.text || "null"); } catch {}
    assert(parsedEcho?.headers?.["x-token"] === "rewritten", "请求头被改写", parsedEcho?.headers);
    assert(sameJson(parsedEcho?.body, { changed: true }), "请求体被改写", parsedEcho?.body);

    console.log("17) fetch 与 XHR 行为一致");
    await run(["net", "reset", ...tab]);
    await run([
      "net", "mock", "/api",
      "--response-status", "202",
      "--response-body", '{"v":9}',
      "--header", "x-mock: fetch",
      ...tab
    ]);
    res = await run([
      "exec",
      "const r = await fetch('/api'); return { status: r.status, header: r.headers.get('x-mock'), json: await r.json() };",
      ...tab
    ]);
    const viaFetch = jsReturn(res);
    assert(viaFetch?.status === 202, "fetch 也读到改写后的 status", viaFetch);
    assert(viaFetch?.header === "fetch", "fetch 也读到改写后的头", viaFetch);
    assert(viaFetch?.json?.v === 9, "fetch 也读到改写后的 body", viaFetch);

    console.log("18) reset 后访问器已还原");
    await run(["net", "reset", ...tab]);
    res = await run(["exec", "return { guarded: !!(window.__abcNetState && window.__abcNetState.xhrGuarded) }", ...tab]);
    assert(jsReturn(res)?.guarded === false, "reset 关掉了访问器守卫", jsReturn(res));
    res = await run(["exec", "return await window.runXhr('/api')", ...tab]);
    const afterReset = jsReturn(res);
    assert(afterReset?.status === 200 && String(afterReset?.text || "").includes('"v":1'), "reset 后 XHR 完全恢复", afterReset);

    console.log("19) 持久 hook 刷新后仍然改写");
    await run(["net", "hook", "--persist", ...tab]);
    await run([
      "net", "mock", "/api",
      "--response-status", "201",
      "--response-body", '{"v":5}',
      ...tab
    ]);
    await run(["reload", ...tab]);
    await run(["wait", "--js", "return typeof window.runXhr === 'function'", "--timeout", "15", ...tab]);
    res = await run(["exec", "return await window.runXhr('/api')", ...tab]);
    const afterReload = jsReturn(res);
    assert(afterReload?.status === 201, "刷新后 status 仍被改写", afterReload);
    assert(sameJson(afterReload?.text, { v: 5 }), "刷新后 body 仍被改写", afterReload?.text);
    await run(["net", "reset", ...tab]);

    console.log("20) 严格 CSP 页（禁 eval）");
    const cspOpen = await run(["open", `${url}csp`, "--timeout", "20"]);
    const cspTabId =
      cspOpen.json?.result?.opened_tab_id || cspOpen.json?.result?.metadata?.raw?.id;
    extraTabId = cspTabId || null;
    const cspTab = ["--tab", String(cspTabId)];
    assert(!!cspTabId, "CSP 页打开成功", cspOpen.json?.result);
    // 文本 / 选择器等待不依赖 eval，CSP 页必须照常可用。
    let cspRes = await run(["wait", "--text", "ready", "--timeout", "6", ...cspTab]);
    const cspText = cspRes.json?.result || cspRes.json;
    assert(cspText?.matched === true, "CSP 页 wait --text 正常", cspText);
    cspRes = await run(["read", "--max-chars", "200", ...cspTab]);
    assert(cspRes.json?.result?.page?.title === "csp page", "CSP 页 read 正常", cspRes.json?.result?.page);
    // wait --js 需要编译谓词：要么走 CDP 回退命中，要么给出可执行的错误，
    // 不能静默超时（页面内 new Function 被 CSP 拒绝是预期内的）。
    cspRes = await run(["wait", "--js", "window.__cspFlag === 1", "--timeout", "4", ...cspTab]);
    const cspJs = cspRes.json?.result || cspRes.json;
    assert(
      cspJs?.matched === true || /CSP|unsafe-eval|CDP/i.test(String(cspJs?.error || "")),
      "CSP 页 wait --js：命中或给出明确原因",
      cspJs
    );

    console.log("21) wait --js 跨导航：注入被销毁后按剩余时间重试");
    await run(["exec", "setTimeout(function(){ location.href = '/echo'; }, 300); return 'nav'", ...tab]);
    const navRes = await run(["wait", "--js", "location.pathname === '/echo'", "--timeout", "10", ...tab]);
    const navWait = navRes.json?.result || navRes.json;
    assert(navWait?.matched === true, "换页后条件命中", navWait);
    assert((navWait?.injections ?? 0) >= 2, "换页触发重试（注入次数 > 1）", navWait);

    console.log("22) responseType=blob（页面引擎：规则给了 body 才能构造新 Blob）");
    const reopen = await run(["open", url, "--timeout", "20"]);
    const tab2Id = reopen.json?.result?.opened_tab_id || reopen.json?.result?.metadata?.raw?.id;
    extraTabId2 = tab2Id || null;
    const tab2 = ["--tab", String(tab2Id)];
    await run(["net", "mock", "/api", "--response-body", '{"v":7}', ...tab2]);
    let blobRes = await run(["exec", "return await window.runXhrBlob('/api')", ...tab2]);
    const blob = jsReturn(blobRes);
    assert(blob?.status === 200, "blob 请求成功", blob);
    assert(sameJson(blob?.text, { v: 7 }), "blob 拿到改写后的文本", blob);
    assert(String(blob?.type || "").includes("json"), "Blob 的 type 沿用 content-type", blob);

    // 只改状态码、不给 body：blob 的原始体读不出来，只能放行并如实标记。
    await run(["net", "mock", "/api", "--response-status", "202", ...tab2]);
    blobRes = await run(["exec", "return await window.runXhrBlob('/api')", ...tab2]);
    const blobRaw = jsReturn(blobRes);
    assert(
      blobRaw?.status === 200 && String(blobRaw?.text || "").includes('"v":1'),
      "只改状态时 blob 体保持原始",
      blobRaw
    );
    const blobLog = await run(["net", "log", "--limit", "10", ...tab2]);
    const skipped = (blobLog.json?.result?.net?.items || []).filter((item) =>
      String(item.response_rewrite_skipped || "").includes("blob")
    );
    assert(skipped.length >= 1, "日志标记 blob 未改写", skipped.slice(-2));

    console.log("23) net --engine cdp：不可用时明确失败且不落规则");
    const cdpRes = await run([
      "net", "mock", "/api", "--engine", "cdp", "--response-status", "201", ...tab2
    ]);
    if (cdpRes.json?.ok === true) {
      assert(cdpRes.json?.result?.net?.engine === "cdp", "本机 CDP 可用，规则已下发", cdpRes.json?.result?.net);
      await run(["net", "reset", ...tab2]);
    } else {
      const cdpErr = String(cdpRes.json?.error || "");
      assert(/CDP|debugger|attach|chrome-extension/i.test(cdpErr), "失败原因指向 CDP/debugger", cdpErr);
      const routesAfterFail = await run(["net", "routes", ...tab2]);
      assert(
        (routesAfterFail.json?.result?.net?.cdp?.count ?? -1) === 0,
        "失败时规则不落库",
        routesAfterFail.json?.result?.net
      );
    }

    console.log("24) 规则按 profile 共享：新标签自动继承");
    await run(["net", "reset", ...tab2]);
    await run(["net", "hook", "--persist", ...tab2]);
    await run(["net", "mock", "/api", "--response-status", "201", "--response-body", '{"v":11}', ...tab2]);
    const persistRoutes = await run(["net", "routes", ...tab2]);
    const persistMatches = persistRoutes.json?.result?.net?.matches || [];
    assert(
      persistMatches.some((item) => String(item).includes("127.0.0.1")) && !persistMatches.includes("<all_urls>"),
      "持久化 matches 默认收窄到当前站点",
      persistMatches
    );
    const tab3res = await run(["open", url, "--timeout", "20"]);
    const tab3Id = tab3res.json?.result?.opened_tab_id || tab3res.json?.result?.metadata?.raw?.id;
    extraTabId3 = tab3Id || null;
    const tab3 = ["--tab", String(tab3Id)];
    await run(["wait", "--js", "return typeof window.runXhr === 'function'", "--timeout", "10", ...tab3]);
    const shared = await run(["exec", "return await window.runXhr('/api')", ...tab3]);
    const seen3 = jsReturn(shared);
    assert(seen3?.status === 201, "新标签继承了 profile 规则", seen3);
    const routes3 = await run(["net", "routes", ...tab3]);
    assert((routes3.json?.result?.net?.count ?? 0) >= 1, "routes 跨标签可见", routes3.json?.result?.net);
    await run(["net", "reset", ...tab3]);
    const sharedReset = await run(["net", "routes", ...tab2]);
    assert((sharedReset.json?.result?.net?.count ?? -1) === 0, "在另一个标签 reset 也生效", sharedReset.json?.result?.net);
  } finally {
    if (extraTabId3 && !KEEP) await run(["close", "--tab", String(extraTabId3)]);
    if (extraTabId2 && !KEEP) await run(["close", "--tab", String(extraTabId2)]);
    if (extraTabId && !KEEP) await run(["close", "--tab", String(extraTabId)]);
    if (tabId && !KEEP) await run(["close", "--tab", String(tabId)]);
    server.close();
  }

  console.log(`\n通过 ${passed}，失败 ${failed}`);
  if (failed > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
