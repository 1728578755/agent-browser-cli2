use super::*;
#[allow(unused_imports)]
use clap::{Args, Parser, Subcommand};

pub(crate) fn run_lookup_command(args: LookupCommand) -> Result<()> {
    ensure_server()?;
    let path = match args.target {
        LookupTarget::Tab(args) => {
            let mut path = format!("/lookup/tab/{}", query_escape(&args.tab));
            let mut query = Vec::new();
            if let Some(browser) = args.browser {
                query.push(format!("browser={}", query_escape(&browser)));
            }
            if let Some(profile) = args.profile {
                query.push(format!("profile={}", query_escape(&profile)));
            }
            if !query.is_empty() {
                path.push('?');
                path.push_str(&query.join("&"));
            }
            path
        }
        LookupTarget::Browser(args) => format!("/lookup/browser/{}", query_escape(&args.browser)),
        LookupTarget::Profile(args) => format!("/lookup/profile/{}", query_escape(&args.profile)),
    };
    print_json(request("GET", &path, None, 30.0)?);
    Ok(())
}

pub(crate) fn run_profile_label_command(args: ProfileLabelCommand) -> Result<()> {
    ensure_server()?;
    match args.action {
        ProfileLabelAction::Set(args) => {
            print_json(request(
                "POST",
                "/profile-label",
                Some(json!({
                    "label": args.label,
                    "profile": args.profile,
                    "browser": args.browser,
                    "switch_tab_id": args.tab,
                })),
                args.timeout,
            )?);
        }
        ProfileLabelAction::Clear(args) => {
            print_json(request(
                "POST",
                "/profile-label",
                Some(json!({
                    "label": null,
                    "profile": args.profile,
                    "browser": args.browser,
                    "switch_tab_id": args.tab,
                })),
                args.timeout,
            )?);
        }
    }
    Ok(())
}

/// `tabs --fields` 的白名单：与 daemon `/tabs` 输出字段一一对应。
/// 旧实现没有字段过滤，智能体在几十个标签的长 JSON 里翻 tab_id 很费 token；
/// 白名单校验让拼错的字段立刻报错（exit 2），而不是静默返回一列 null。
const TABS_FIELD_WHITELIST: &[&str] = &[
    "id",
    "tab_id",
    "window",
    "browser_id",
    "browser_label",
    "profile_id",
    "profile_label",
    "session_key",
    "url",
    "title",
    "type",
    "connected_at",
    "group",
    "active",
];

fn parse_tabs_fields(spec: Option<&str>) -> Result<Option<Vec<String>>> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let mut fields = Vec::new();
    for raw in spec.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if !TABS_FIELD_WHITELIST.contains(&name) {
            return Err(usage_failure(
                "unsupported_flag",
                format!("tabs --fields 不支持字段 {name:?}"),
                format!("可用字段：{}", TABS_FIELD_WHITELIST.join(",")),
            ));
        }
        fields.push(name.to_string());
    }
    if fields.is_empty() {
        return Err(usage_failure(
            "unsupported_flag",
            "tabs --fields 至少要给一个字段",
            format!(
                "例如 --fields tab_id,title,url；可用字段：{}",
                TABS_FIELD_WHITELIST.join(",")
            ),
        ));
    }
    Ok(Some(fields))
}

/// 在 daemon 返回上做字段过滤与截断（CLI 侧做，daemon 保持通用）。
///
/// 旧实现 tabs 永远回全部字段全量列表：大浏览器里一次输出几千 token，
/// 智能体其实只用 tab_id/title/url。这里统一处理 full / --ids 两条路径共用的
/// `/result/metadata/tabs`，并把 tabs_count 等元数据同步修正。
fn apply_fields_limit(value: &mut Value, fields: Option<&[String]>, limit: Option<usize>) {
    let Some(tabs) = value
        .pointer("/result/metadata/tabs")
        .and_then(Value::as_array)
    else {
        return;
    };
    let total = tabs.len();
    let mut items: Vec<Value> = tabs
        .iter()
        .map(|tab| match fields {
            Some(fields) => {
                let mut item = serde_json::Map::new();
                for field in fields {
                    item.insert(
                        field.clone(),
                        tab.get(field).cloned().unwrap_or(Value::Null),
                    );
                }
                Value::Object(item)
            }
            None => tab.clone(),
        })
        .collect();
    if let Some(limit) = limit {
        items.truncate(limit);
    }
    if let Some(metadata) = value
        .pointer_mut("/result/metadata")
        .and_then(Value::as_object_mut)
    {
        metadata.insert("tabs_count".to_string(), json!(items.len()));
        if let Some(fields) = fields {
            metadata.insert("fields".to_string(), json!(fields));
        }
        if let Some(limit) = limit {
            metadata.insert("limit".to_string(), json!(limit));
            metadata.insert("total_before_limit".to_string(), json!(total));
        }
    }
    if let Some(slots) = value
        .pointer_mut("/result/metadata/tabs")
        .and_then(Value::as_array_mut)
    {
        *slots = items;
    }
}

pub(crate) fn cmd_tabs(args: TabsArgs, is_async: bool) -> Result<()> {
    reject_async_if_set("tabs", is_async)?;
    // 字段白名单在连接 daemon 之前校验：写错了直接 exit 2，不产生任何副作用。
    let fields = parse_tabs_fields(args.fields.as_deref())?;
    ensure_server()?;
    let mut path = "/tabs".to_string();
    let mut query = Vec::new();
    if let Some(browser) = args.browser {
        query.push(format!("browser={}", query_escape(&browser)));
    }
    if let Some(profile) = args.profile {
        query.push(format!("profile={}", query_escape(&profile)));
    }
    if args.full {
        query.push("full=true".to_string());
    }
    if args.mine {
        // mine=true 由 daemon 过滤：只有 daemon 知道哪些会话是本 agent open 的
        // （agent_opened 记录），CLI 侧猜不出来。
        query.push("mine=true".to_string());
    }
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    let mut value = request("GET", &path, None, 30.0)?;
    let trimming = fields.is_some() || args.limit.is_some();
    if !args.ids {
        if trimming {
            // 旧实现带不带 --ids 都回全量字段；现在 --fields/--limit 在 CLI 侧裁剪，
            // daemon 返回结构不变，没给这两个 flag 时输出与旧版完全一致。
            apply_fields_limit(&mut value, fields.as_deref(), args.limit);
        }
        print_json(value);
        return Ok(());
    }
    // `ok:false` 时必须原样带出错误与 error_code：以前只搬了 ok，再附加一条与错误无关的
    // hint，把 Agent 引向「继续选目标」而不是「先修问题」。
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        print_json(value);
        return Ok(());
    }
    // 精简视图：只留选目标要用的字段，省 token 也省得智能体在长 JSON 里找 tab_id。
    let tabs = value
        .pointer("/result/metadata/tabs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // 精简视图的字段集合：`--fields` 直接作用于**原始 tab 对象**，`--ids` 只是
    // 默认字段集的别名。旧实现先在精简视图里裁到 6 个字段，再应用 --fields，
    // 于是 `tabs --ids --fields session_key` 恒为 null——字段根本没被带出来。
    const IDS_DEFAULT_FIELDS: &[&str] = &["tab_id", "window", "title", "url", "active", "group"];
    let selected: Vec<String> = match &fields {
        Some(fields) => fields.clone(),
        None => IDS_DEFAULT_FIELDS
            .iter()
            .map(|field| field.to_string())
            .collect(),
    };
    let mut items: Vec<Value> = tabs
        .iter()
        .map(|tab| {
            let mut item = serde_json::Map::new();
            for field in &selected {
                item.insert(
                    field.clone(),
                    tab.get(field).cloned().unwrap_or(Value::Null),
                );
            }
            Value::Object(item)
        })
        .collect();
    if let Some(limit) = args.limit {
        items.truncate(limit);
    }
    print_json(json!({
        "ok": value.get("ok").cloned().unwrap_or(json!(false)),
        "count": items.len(),
        "tabs": items,
        "hint": "把某个 tab_id 直接传给 --tab；也可以 --tab @1 按序号、--tab window:<id> 按窗口",
    }));
    Ok(())
}

pub(crate) fn cmd_tabtree(args: TabtreeArgs, is_async: bool) -> Result<()> {
    reject_async_if_set("tabtree", is_async)?;
    ensure_server()?;
    let mut path = "/tabtree".to_string();
    let mut query = Vec::new();
    if let Some(tab) = args.tab {
        query.push(format!("tab={}", query_escape(&tab)));
    }
    if let Some(browser) = args.browser {
        query.push(format!("browser={}", query_escape(&browser)));
    }
    if let Some(profile) = args.profile {
        query.push(format!("profile={}", query_escape(&profile)));
    }
    if args.full {
        query.push("full=true".to_string());
    }
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    print_json(request("GET", &path, None, 30.0)?);
    Ok(())
}
