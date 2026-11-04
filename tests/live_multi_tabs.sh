#!/usr/bin/env bash
# 多标签并发端到端验证（需要真实 Chrome + 已连接的扩展）。
#
# 用法：
#   bash tests/live_multi_tabs.sh ./target/release/atb.exe
#
# 覆盖：并发打开、多标签 exec/read、@N 序号、顺序稳定性、目标不存在的报错与退出码、
#       --dry-run、预检拦截、输出路径冲突、批量关闭。会自行清理创建的测试标签页。
set -u
B="${1:-./target/release/atb.exe}"
pass=0; fail=0
now() { date +%s%3N; }
check() { # name expected actual
  if [ "$2" = "$3" ]; then echo "PASS  $1"; pass=$((pass+1));
  else echo "FAIL  $1  expected=$2 actual=$3"; fail=$((fail+1)); fi
}
jget() { python -c "import json,sys;d=json.load(sys.stdin);print(eval(sys.argv[1]))" "$1"; }

echo "== 1. 并发打开 6 个标签 =="
t0=$(now)
"$B" open "https://example.com/?e2e=1" "https://example.com/?e2e=2" "https://example.com/?e2e=3" \
        "https://example.com/?e2e=4" "https://example.com/?e2e=5" "https://example.com/?e2e=6" \
        --background > /tmp/e2e_open.json 2>&1
t1=$(now)
echo "  open6 总耗时: $((t1-t0))ms"
check "open 成功" "True" "$(jget "d['ok']" < /tmp/e2e_open.json)"
check "open 数量" "6" "$(jget "len(d['tabs'])" < /tmp/e2e_open.json)"
check "全部 ready" "6" "$(jget "sum(1 for t in d['tabs'] if t['ready'])" < /tmp/e2e_open.json)"
IDS=$(jget "','.join(t['tab_id'] for t in d['tabs'])" < /tmp/e2e_open.json)
echo "  tab ids: $IDS"

echo "== 2. 多标签并发 exec =="
t0=$(now)
"$B" exec --tab "url:e2e=" 'return document.title' > /tmp/e2e_exec.json 2>&1
t1=$(now)
echo "  exec6 总耗时: $((t1-t0))ms (daemon elapsed $(jget "d['elapsed_ms']" < /tmp/e2e_exec.json)ms)"
check "exec 命中 6 个" "6" "$(jget "d['count']" < /tmp/e2e_exec.json)"
check "exec 全部成功" "0" "$(jget "d['failed']" < /tmp/e2e_exec.json)"
check "exec 结果正确" "Example Domain" "$(jget "d['results'][0]['result']['js_return']" < /tmp/e2e_exec.json)"

echo "== 3. 串行对照（6 次独立进程）=="
t0=$(now)
for id in ${IDS//,/ }; do "$B" exec --tab "$id" 'return document.title' > /dev/null 2>&1; done
t1=$(now)
echo "  serial6 总耗时: $((t1-t0))ms"

echo "== 4. 多标签 read =="
"$B" read --tab "url:e2e=" --max-chars 400 > /tmp/e2e_read.json 2>&1
check "read 命中 6 个" "6" "$(jget "d['count']" < /tmp/e2e_read.json)"
check "read 全部成功" "0" "$(jget "d['failed']" < /tmp/e2e_read.json)"
check "read 拿到标题" "Example Domain" "$(jget "d['results'][0]['result']['page']['title']" < /tmp/e2e_read.json)"

echo "== 5. 按序号 @N 选择 =="
"$B" tabs --ids > /tmp/e2e_tabs.json 2>&1
first=$(jget "d['tabs'][0]['tab_id']" < /tmp/e2e_tabs.json)
"$B" exec --tab @1 'return 1' --dry-run > /tmp/e2e_at1.json 2>&1
check "@1 指向 tabs 第一项" "$first" "$(jget "d['targets'][0]['tab_id']" < /tmp/e2e_at1.json)"

echo "== 6. tabs 顺序稳定性 =="
a=$("$B" tabs --ids | jget "','.join(t['tab_id'] for t in d['tabs'])")
b=$("$B" tabs --ids | jget "','.join(t['tab_id'] for t in d['tabs'])")
check "两次 tabs 顺序一致" "same" "$([ "$a" = "$b" ] && echo same || echo diff)"

echo "== 7. 目标不存在：候选列表 + 退出码 2 =="
"$B" read --tab 99999999 > /tmp/e2e_bad.json 2>&1
code=$?
check "退出码为 2" "2" "$code"
check "错误码 target_not_found" "target_not_found" "$(jget "d.get('error_code')" < /tmp/e2e_bad.json)"
echo "  报错: $(jget "d['error'][:80]" < /tmp/e2e_bad.json)..."
echo "  提示: $(jget "d.get('hint','')[:60]" < /tmp/e2e_bad.json)"

echo "== 8. dry-run 不动页面 =="
"$B" exec --tab "url:e2e=" 'return 42' --dry-run > /tmp/e2e_dry.json 2>&1
check "dry_run 标记" "True" "$(jget "d['dry_run']" < /tmp/e2e_dry.json)"
check "dry_run 解析 6 个目标" "6" "$(jget "d['count']" < /tmp/e2e_dry.json)"

echo "== 9. 预检拦截（JS 括号）=="
"$B" exec 'if (x) { return 1' > /tmp/e2e_pf.json 2>&1
code=$?
check "预检退出码 2" "2" "$code"
check "错误码 invalid_js" "invalid_js" "$(jget "d['error_code']" < /tmp/e2e_pf.json)"

echo "== 10. 输出路径冲突拦截 =="
"$B" screenshot --tab "url:e2e=" --out /tmp/shot.png > /tmp/e2e_out.json 2>&1
code=$?
check "冲突退出码 2" "2" "$code"
check "错误码 output_path_collision" "output_path_collision" "$(jget "d['error_code']" < /tmp/e2e_out.json)"

echo "== 11. 清理测试标签 =="
"$B" close --tab "$IDS" > /tmp/e2e_close.json 2>&1
check "关闭 6 个成功" "True" "$(jget "d['ok']" < /tmp/e2e_close.json)"

echo
echo "结果: PASS=$pass FAIL=$fail"
[ "$fail" -eq 0 ]
