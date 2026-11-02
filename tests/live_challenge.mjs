// 真实浏览器验收：挑战页 / 验证码识别（challenge status|wait|focus）。
//
// 用本地假页面模拟三类现场：
//   /plain         普通页面            → 不该误报
//   /cf            CF 拦截页（1.5s 后自动放行） → 识别为 cloudflare-interstitial 且 wait 能等到清除
//   /cf-stuck      CF 拦截页（永不放行）  → wait 超时返回 cleared:false，且能要求转人工
//   /form-captcha  带 reCAPTCHA 组件的登录页 → 识别为 widget，不阻断（wait 立即 cleared）
//
// 运行：node tests/live_challenge.mjs [--tab <id>] [--browser <id>] [--keep]
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

let passed = 0;
let failed = 0;
function assert(cond, name, detail) {
  if (cond) {
    passed += 1;
    console.log(`  PASS  ${name}`);
  } else {
    failed += 1;
    console.log(`  FAIL  ${name}`);
    if (detail !== undefined) console.log("        " + JSON.stringify(detail).slice(0, 700));
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

const CF_BODY = `<div id="cf-challenge-running">
  <h2>Checking your browser before accessing the site</h2>
  <p>This process is automatic. Your browser will redirect shortly.</p>
</div>`;

function page(title, body, script = "") {
  return `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head>
<body>${body}<script>${script}</script></body></html>`;
}

function serve() {
  const server = http.createServer((req, res) => {
    const url = String(req.url || "");
    const send = (html) => {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      res.end(html);
    };
    if (url.startsWith("/plain")) {
      send(page("Plain page", "<h1>hello</h1><p>nothing to verify here</p>"));
      return;
    }
    if (url.startsWith("/cf-stuck")) {
      send(page("Just a moment...", CF_BODY));
      return;
    }
    if (url.startsWith("/cf")) {
      // 模拟 CF 的 JS 挑战通过：换标题 + 换正文。延迟可用 ?delay=ms 指定，
      // 测试用长一点，才能验证「确实等了」而不是瞬间返回。
      const delay = Number(new URL(url, "http://127.0.0.1").searchParams.get("delay") || 1500);
      send(
        page(
          "Just a moment...",
          CF_BODY,
          `setTimeout(function(){ document.title='Real page'; document.body.innerHTML='<h1>welcome</h1><p>content</p>'; }, ${delay});`
        )
      );
      return;
    }
    if (url.startsWith("/turnstile-token")) {
      // 模拟 Turnstile：组件在场，token 1.2 秒后（人点完 / 隐形挑战自动完成）写入
      send(page(
        "Login",
        `<div class="cf-turnstile" data-sitekey="demo"></div><input type="hidden" name="cf-turnstile-response" value=""><p>form</p>`,
        "setTimeout(function(){ var el=document.getElementsByName('cf-turnstile-response')[0]; if(el) el.value='tok_abc123'; }, 2500);"
      ));
      return;
    }
    if (url.startsWith("/slider-aliyun")) {
      send(page("安全验证", `<div class="nc-container"><span id="nc_1_n1z">滑块</span></div>`));
      return;
    }
    if (url.startsWith("/slider-geetest")) {
      send(page("验证", `<div class="geetest_holder"><div class="geetest_slider"></div></div>`));
      return;
    }
    if (url.startsWith("/form-captcha")) {
      send(
        page(
          "Login",
          `<form><input name="user"><div class="g-recaptcha" data-sitekey="demo"></div><button>登录</button></form>`
        )
      );
      return;
    }
    send(page("Plain page", "<h1>hello</h1>"));
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve({ server, port: server.address().port }));
  });
}

const payloadOf = (result) => result.json?.result ?? result.json;

async function main() {
  const { server, port } = await serve();
  const base = `http://127.0.0.1:${port}`;
  let tabId = wantTab;
  console.log(`本地服务: ${base}`);

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

    // 复用同一个标签页导航，避免每换一个场景就多开一个标签（open --tab 会新开）。
    const openPage = async (suffix) => {
      const target = `${base}${suffix}`;
      if (!tabId) {
        const args = ["open", target, "--timeout", "20"];
        if (wantBrowser) args.push("--browser", wantBrowser);
        const res = await run(args);
        tabId = res.json?.result?.opened_tab_id || res.json?.result?.metadata?.raw?.id;
      } else {
        await run(["exec", `location.href = ${JSON.stringify(target)}; return 1`, "--tab", String(tabId)]);
      }
      return ["--tab", String(tabId)];
    };

    console.log("1) 普通页面不误报");
    let tab = await openPage("/plain");
    await run(["wait", "--js", "location.href.includes('/plain') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    let res = await run(["challenge", "status", ...tab]);
    let ch = payloadOf(res)?.challenge;
    assert(ch?.detected === false, "普通页面 detected=false", ch);
    assert(Array.isArray(ch?.hints) && ch.hints.length >= 1, "给出 hints", ch);

    console.log("2) 识别 CF 拦截页");
    tab = await openPage("/cf-stuck");
    await run(["wait", "--js", "location.href.includes('/cf-stuck') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "status", ...tab]);
    ch = payloadOf(res)?.challenge;
    assert(ch?.detected === true, "识别出挑战页", ch);
    assert(ch?.vendor === "cloudflare-interstitial", "vendor=cloudflare-interstitial", ch?.vendor);
    assert(ch?.blocking === true && ch?.auto === true, "blocking 且可等待自动放行", ch);
    assert(Array.isArray(ch?.signals) && ch.signals.length >= 1, "带上命中的证据", ch?.signals);

    console.log("3) read 里带出 challenge 字段");
    res = await run(["read", "--max-chars", "500", ...tab]);
    const readPayload = payloadOf(res);
    assert(readPayload?.challenge?.detected === true, "read 结果附带挑战信息", readPayload?.challenge);
    assert(readPayload?.page?.title?.includes("Just a moment"), "仍然是原页面标题", readPayload?.page?.title);

    console.log("4) wait 等到自动放行（1.5s 后）");
    // 自动放行设在 8 秒后：前面的就绪等待即便花掉两三秒，wait 仍要真等一段。
    tab = await openPage("/cf?delay=8000");
    await run(["wait", "--js", "location.href.includes('delay=8000') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "wait", "--timeout", "20", ...tab]);
    const waited = payloadOf(res);
    assert(waited?.cleared === true, "挑战已清除", waited);
    // 这里不断言耗时：前面等就绪/导航已经吃掉一部分时间，页面可能刚好在这期间放行。
    // 「确实等待」由第 5 步（永不放行页面必须超时）来保证。
    assert((waited?.challenge?.elapsed_ms ?? 0) >= 200, "wait 内部有等待窗口（稳定确认）", waited?.challenge);
    assert((waited?.injections ?? 0) >= 1, "有注入记录", waited?.injections);

    console.log("5) 永不放行的页面：超时返回 cleared:false");
    tab = await openPage("/cf-stuck");
    await run(["wait", "--js", "location.href.includes('/cf-stuck') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "wait", "--timeout", "2", "--focus-after", "0", "--no-focus", ...tab]);
    const stuck = payloadOf(res);
    assert(stuck?.cleared === false, "超时未清除", stuck);
    assert(stuck?.challenge?.blocking === true, "仍报告被拦截", stuck?.challenge);

    console.log("6) focus：只激活标签页，不抢系统焦点");
    res = await run(["challenge", "focus", "--no-focus", ...tab]);
    const focused = payloadOf(res);
    assert(focused?.focused === true && focused?.action === "focus", "focus 返回成功", focused);

    console.log("7) 带 reCAPTCHA 的表单页：识别为组件、不阻断");
    tab = await openPage("/form-captcha");
    await run(["wait", "--js", "location.href.includes('/form-captcha') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "status", ...tab]);
    ch = payloadOf(res)?.challenge;
    assert(ch?.detected === true && ch?.kind === "widget", "识别为 widget", ch);
    assert(ch?.vendor === "recaptcha", "vendor=recaptcha", ch?.vendor);
    assert(ch?.blocking === false && ch?.interactive === true, "不阻断但需要人", ch);
    res = await run(["challenge", "wait", "--timeout", "5", ...tab]);
    const widgetWait = payloadOf(res);
    assert(widgetWait?.cleared === true && (widgetWait?.elapsed_ms ?? 9999) < 2000, "组件页 wait 立即返回", widgetWait);
    console.log("8) 滑块类验证码：识别为 slider，不当作可自动放行");
    tab = await openPage("/slider-aliyun");
    await run(["wait", "--js", "location.href.includes('/slider-aliyun') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "status", ...tab]);
    ch = payloadOf(res)?.challenge;
    assert(ch?.vendor === "aliyun-nocaptcha" && ch?.kind === "slider", "识别阿里云滑块", ch);
    assert(ch?.blocking === false && ch?.interactive === true, "滑块不阻断但需要人", ch);
    assert(/滑块|拖动|点选/.test(String(ch?.hints || "")), "提示里说明要人工拖动", ch?.hints);

    tab = await openPage("/slider-geetest");
    await run(["wait", "--js", "location.href.includes('/slider-geetest') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "status", ...tab]);
    ch = payloadOf(res)?.challenge;
    assert(ch?.vendor === "geetest" && ch?.kind === "slider", "识别极验滑块", ch);
    console.log("9) solve：等 token / 等放行 / 没挑战时立即返回");
    tab = await openPage("/turnstile-token");
    await run(["wait", "--js", "location.href.includes('/turnstile-token') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "status", ...tab]);
    ch = payloadOf(res)?.challenge;
    // 不断言「初始没有 token」：等页面就绪 + CLI 启动可能已经超过 token 出现的时刻。
    assert(ch?.vendor === "turnstile" && Array.isArray(ch?.token_fields), "识别 turnstile 并暴露 token 字段", ch);
    res = await run(["challenge", "solve", "--timeout", "10", "--no-focus", ...tab]);
    const solvedToken = payloadOf(res);
    assert(solvedToken?.passed === true && solvedToken?.via === "token", "solve 拿到 token 后通过", solvedToken);
    assert((solvedToken?.challenge?.token_fields || []).some((f) => f.length > 0), "token 字段有值", solvedToken?.challenge?.token_fields);

    tab = await openPage("/plain");
    await run(["wait", "--js", "location.href.includes('/plain') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "solve", "--timeout", "10", "--no-focus", ...tab]);
    const solvedNone = payloadOf(res);
    assert(solvedNone?.passed === true && solvedNone?.via === "no-challenge", "没有挑战时立即通过", solvedNone);

    tab = await openPage("/cf-stuck");
    await run(["wait", "--js", "location.href.includes('/cf-stuck') && document.readyState === 'complete'", "--timeout", "15", ...tab]);
    res = await run(["challenge", "solve", "--timeout", "2", "--focus-after", "0", "--no-focus", ...tab]);
    const solveTimeout = payloadOf(res);
    assert(solveTimeout?.passed === false && solveTimeout?.reason === "timeout", "永不放行时明确失败", solveTimeout);
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
