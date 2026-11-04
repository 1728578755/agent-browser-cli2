//! `--tab` 目标规格解析。
//!
//! 同一个参数既要能表达「就这个标签页」，也要能表达「这一批标签页」。语法故意保持
//! 扁平，全部写在 `--tab` 一个参数里，不引入第二套选择器概念：
//!
//! ```text
//! --tab 1234567890              单个 tab id（原有行为，向后兼容）
//! --tab 123,456                 多个 tab id
//! --tab all                     当前 profile 下所有可操作标签页
//! --tab current                 浏览器当前激活（可见）的标签页
//! --tab @1,@3                   `tabs` / `tabtree` 输出里的序号（1 起）
//! --tab url:example.com         按 URL 子串匹配（多个命中就并发）
//! --tab title:订单               按标题子串匹配
//! --tab group:research          按 Chrome 标签组标题匹配
//! ```
//!
//! 这里只做**语法**解析，不访问 daemon：语法错误能在进程内立刻报出来，
//! 不需要发一次请求再等错误返回。真正的存在性校验由 daemon 的 `/multi` 完成。

use std::fmt;

/// 一个 `--tab` 规格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSpec {
    /// 没写 `--tab`：沿用 daemon 的默认标签页。
    Default,
    /// 所有可操作标签页。
    All,
    /// 浏览器当前激活的标签页。
    Current,
    /// `tabs` 输出里的 1-based 序号。
    Index(Vec<usize>),
    /// URL 子串（大小写不敏感）。
    Url(String),
    /// 标题子串（大小写不敏感）。
    Title(String),
    /// Chrome 标签组标题（精确匹配，大小写不敏感）。
    Group(String),
    /// Chrome 窗口 id：同一个 Profile 可以开多个窗口。
    Window(i64),
    /// tab id 或完整 session_key。
    Ids(Vec<String>),
}

/// 语法错误的稳定描述。
///
/// `code` 进 JSON 的 `error_code`，`hint` 必须是「下一步该怎么做」，
/// 而不是把错误换个说法重复一遍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
    pub example: &'static str,
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SpecError {}

const EXAMPLE: &str = "atb read --tab all";

impl TargetSpec {
    /// 解析 `--tab` 的取值。空字符串按「未指定」处理（shell 变量展开常见）。
    pub fn parse(raw: &str) -> Result<Self, SpecError> {
        let text = raw.trim();
        if text.is_empty() {
            return Ok(TargetSpec::Default);
        }
        let lower = text.to_ascii_lowercase();
        match lower.as_str() {
            "all" | "*" => return Ok(TargetSpec::All),
            "current" | "active" => return Ok(TargetSpec::Current),
            _ => {}
        }
        if let Some(rest) = strip_prefix_ci(text, "url:") {
            return non_empty(rest, "url:").map(TargetSpec::Url);
        }
        if let Some(rest) = strip_prefix_ci(text, "title:") {
            return non_empty(rest, "title:").map(TargetSpec::Title);
        }
        if let Some(rest) = strip_prefix_ci(text, "group:") {
            return non_empty(rest, "group:").map(TargetSpec::Group);
        }
        if let Some(rest) = strip_prefix_ci(text, "window:") {
            let id = non_empty(rest, "window:")?;
            return id
                .parse::<i64>()
                .map(TargetSpec::Window)
                .map_err(|_| SpecError {
                    code: "invalid_target_spec",
                    message: format!("--tab window:{id} 不是有效的窗口 id"),
                    hint: "窗口 id 是数字，见 `tabs --ids` 每项的 window 字段".to_string(),
                    example: "atb read --tab window:1234567",
                });
        }
        if let Some(rest) = text.strip_prefix('@').or_else(|| text.strip_prefix('#')) {
            return parse_indexes(rest);
        }
        let parts: Vec<String> = text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
        if parts.is_empty() {
            return Err(SpecError {
                code: "invalid_target_spec",
                message: format!("--tab 取值 {raw:?} 里没有有效的标签页标识"),
                hint: "逗号分隔里至少写一个 tab id；只有一个目标时直接写 `--tab <tabId>`"
                    .to_string(),
                example: EXAMPLE,
            });
        }
        for part in &parts {
            if part.chars().any(char::is_whitespace) {
                return Err(SpecError {
                    code: "invalid_target_spec",
                    message: format!(
                        "--tab 里的 {part:?} 含空格；tab id 和 session_key 都不含空格"
                    ),
                    hint: "如果是从别处复制的整行输出，先用 `tabs --ids` 取纯 id 再传".to_string(),
                    example: "atb tabs --ids",
                });
            }
            if !part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
            {
                return Err(SpecError {
                    code: "invalid_target_spec",
                    message: format!("--tab 里的 {part:?} 不是合法的 tab id / session_key"),
                    hint: "tab id 是纯数字；session_key 形如 browser-x:profile-y:123。\
                           要用序号写 `--tab @1`；要按 URL 写 `--tab url:example.com`"
                        .to_string(),
                    example: "atb read --tab @1",
                });
            }
        }
        // 去重按原顺序（保留首次出现）：`--tab 5,5` 只该命中一个标签页，否则同一条
        // 命令会对同一个页面执行两遍（点击两次、填两次）。daemon 侧已有去重，这里是
        // 双保险。
        Ok(TargetSpec::Ids(dedupe(parts)))
    }

    /// 是否必须由 daemon 解析目标。
    ///
    /// `@1` / `url:` / `title:` / `group:` / `all` / `current` 都只能在 daemon 侧
    /// 换算成具体会话；即使最终只命中一个标签页，也必须走 `/multi` 的解析路径，
    /// 否则 CLI 会把 `@1` 当成 tab id 直接发下去，必然报「未连接」。
    pub fn needs_daemon_resolution(&self) -> bool {
        matches!(
            self,
            TargetSpec::All
                | TargetSpec::Current
                | TargetSpec::Index(_)
                | TargetSpec::Url(_)
                | TargetSpec::Title(_)
                | TargetSpec::Group(_)
                | TargetSpec::Window(_)
        )
    }

    /// 是否可能打到多个标签页（决定 `--async` 是否可用）。
    pub fn is_multi(&self) -> bool {
        match self {
            TargetSpec::Default | TargetSpec::Current => false,
            TargetSpec::All
            | TargetSpec::Url(_)
            | TargetSpec::Title(_)
            | TargetSpec::Group(_)
            | TargetSpec::Window(_) => true,
            TargetSpec::Index(indexes) => indexes.len() > 1,
            TargetSpec::Ids(ids) => ids.len() > 1,
        }
    }

    /// 单标签路径下真正要发给 daemon 的值（多目标返回 None）。
    ///
    /// 仅测试使用：单目标捷径。
    #[cfg(test)]
    pub fn single_id(&self) -> Option<&str> {
        match self {
            TargetSpec::Ids(ids) if ids.len() == 1 => Some(ids[0].as_str()),
            _ => None,
        }
    }

    /// 人读描述，用于 `--dry-run` 和错误提示。
    pub fn describe(&self) -> String {
        match self {
            TargetSpec::Default => "default".to_string(),
            TargetSpec::All => "all".to_string(),
            TargetSpec::Current => "current".to_string(),
            TargetSpec::Index(values) => {
                let list: Vec<String> = values.iter().map(|v| format!("@{v}")).collect();
                list.join(",")
            }
            TargetSpec::Url(value) => format!("url:{value}"),
            TargetSpec::Title(value) => format!("title:{value}"),
            TargetSpec::Group(value) => format!("group:{value}"),
            TargetSpec::Window(value) => format!("window:{value}"),
            TargetSpec::Ids(ids) => ids.join(","),
        }
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    // 切片前必须确认落在**字符边界**上。`prefix` 恒为 ASCII（`all` / `url:` / `title:`
    // / `group:` / `window:` / `current`），但 `text` 可能紧接着一个多字节字符：
    // `--tab "all中"` 时 `split_at(4)` 会落在 '中'（3..6 字节）中间，直接 panic 到
    // exit 101 —— 101 不在对外退出码契约（1/2/3/130）里，Agent 无法机器判读。
    // `str::get(..n)` 在越界或非边界时返回 None，正好等价于「前缀不匹配」。
    let head = text.get(..prefix.len())?;
    let rest = text.get(prefix.len()..)?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

fn non_empty(value: &str, prefix: &str) -> Result<String, SpecError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(SpecError {
            code: "invalid_target_spec",
            message: format!("--tab {prefix} 后面必须跟匹配内容"),
            hint: format!("例如 `--tab {prefix}example.com`（子串匹配，不区分大小写）"),
            example: EXAMPLE,
        });
    }
    Ok(trimmed.to_string())
}

fn parse_indexes(raw: &str) -> Result<TargetSpec, SpecError> {
    let mut indexes = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        // 支持 `@1-3` 这种连续区间，省得手写一长串。
        if let Some((start, end)) = token.split_once('-') {
            let start = parse_index(start)?;
            let end = parse_index(end)?;
            if start > end {
                return Err(SpecError {
                    code: "invalid_target_spec",
                    message: format!("--tab @{token} 的区间起点大于终点"),
                    hint: "写成 `@1-3` 这样从小到大；序号从 1 开始".to_string(),
                    example: "atb read --tab @1-3",
                });
            }
            // 上限保护：`@1-1000000000`（多敲几位）会直接分配数 GB 内存并 abort，
            // 而且发生在预检阶段，浏览器完全无感。
            //
            // 跨度必须全程 checked：`end - start + 1` 在 `@1-18446744073709551615`
            // （usize::MAX）时会整数溢出 —— debug 构建直接 panic 到 exit 101（不在对外
            // 退出码契约里），release 构建回绕成 0 反而绕过上限检查。
            const MAX_INDEX_SPAN: usize = 512;
            let span = end
                .checked_sub(start)
                .and_then(|distance| distance.checked_add(1))
                .ok_or_else(|| SpecError {
                    code: "invalid_target_spec",
                    message: format!("--tab @{token} 的区间跨度超出可表示范围"),
                    hint: "先 `tabs --ids` 看实际有多少标签页，再按需要写区间".to_string(),
                    example: "atb read --tab @1-5",
                })?;
            if span > MAX_INDEX_SPAN {
                return Err(SpecError {
                    code: "invalid_target_spec",
                    message: format!(
                        "--tab @{token} 的区间包含 {span} 个序号，超过上限 {MAX_INDEX_SPAN}"
                    ),
                    hint: "先 `tabs --ids` 看实际有多少标签页，再按需要写区间".to_string(),
                    example: "atb read --tab @1-5",
                });
            }
            indexes.extend(start..=end);
            continue;
        }
        indexes.push(parse_index(token)?);
    }
    if indexes.is_empty() {
        return Err(SpecError {
            code: "invalid_target_spec",
            message: "--tab @ 后面缺少序号".to_string(),
            hint: "写 `--tab @1` 表示 tabs 列表里的第一个标签页（从 1 开始）".to_string(),
            example: "atb tabs --ids && atb read --tab @1",
        });
    }
    Ok(TargetSpec::Index(dedupe(indexes)))
}

/// 按原顺序去重，保留首次出现（标签页数量很小，O(n²) 足够）。
fn dedupe<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn parse_index(raw: &str) -> Result<usize, SpecError> {
    let value = raw.trim().parse::<usize>().map_err(|_| SpecError {
        code: "invalid_target_spec",
        message: format!("--tab @{raw} 不是有效序号"),
        hint: "序号必须是 1 起的整数（`@1`、`@1,@2`、`@1-3`）".to_string(),
        example: "atb tabs --ids",
    })?;
    // `@0` 语法上能解析成 usize，但序号是 1-based：放过去的话 daemon 侧会算成
    // 「第 0 个标签页」或直接命中数组边界，报出来的错误与真实原因无关。
    if value == 0 {
        return Err(SpecError {
            code: "invalid_target_spec",
            message: format!("--tab @{raw} 的序号是 0，但序号从 1 开始"),
            hint: "第一个标签页写 `@1`；想看完整列表用 `atb tabs`".to_string(),
            example: "atb read --tab @1",
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_ids_are_collapsed() {
        // `--tab 5,5` 只该命中一个标签页；顺序按首次出现保留。
        assert_eq!(
            TargetSpec::parse("5,5").unwrap(),
            TargetSpec::Ids(vec!["5".into()])
        );
        assert_eq!(
            TargetSpec::parse("7, 5, 7").unwrap(),
            TargetSpec::Ids(vec!["7".into(), "5".into()])
        );
        // 序号列表同样去重；区间与单点重复时也只保留一次。
        assert_eq!(
            TargetSpec::parse("@1,1").unwrap(),
            TargetSpec::Index(vec![1])
        );
        assert_eq!(
            TargetSpec::parse("@1-3,2").unwrap(),
            TargetSpec::Index(vec![1, 2, 3])
        );
    }

    #[test]
    fn single_id_stays_single() {
        let spec = TargetSpec::parse("1163421542").unwrap();
        assert_eq!(spec, TargetSpec::Ids(vec!["1163421542".into()]));
        assert!(!spec.is_multi());
        assert_eq!(spec.single_id(), Some("1163421542"));
    }

    #[test]
    fn comma_list_is_multi() {
        let spec = TargetSpec::parse("111, 222").unwrap();
        assert_eq!(spec, TargetSpec::Ids(vec!["111".into(), "222".into()]));
        assert!(spec.is_multi());
        assert_eq!(spec.single_id(), None);
    }

    #[test]
    fn keywords() {
        assert_eq!(TargetSpec::parse("ALL").unwrap(), TargetSpec::All);
        assert_eq!(TargetSpec::parse("current").unwrap(), TargetSpec::Current);
        assert_eq!(TargetSpec::parse("").unwrap(), TargetSpec::Default);
    }

    #[test]
    fn prefix_selectors() {
        assert_eq!(
            TargetSpec::parse("url:example.com").unwrap(),
            TargetSpec::Url("example.com".into())
        );
        assert_eq!(
            TargetSpec::parse("title:订单").unwrap(),
            TargetSpec::Title("订单".into())
        );
        assert_eq!(
            TargetSpec::parse("group:research").unwrap(),
            TargetSpec::Group("research".into())
        );
        assert!(TargetSpec::parse("url:").is_err());
    }

    #[test]
    fn window_selector() {
        assert_eq!(
            TargetSpec::parse("window:1234567").unwrap(),
            TargetSpec::Window(1234567)
        );
        assert!(TargetSpec::parse("window:abc").is_err());
    }

    #[test]
    fn indexes_and_ranges() {
        assert_eq!(TargetSpec::parse("@1").unwrap(), TargetSpec::Index(vec![1]));
        assert_eq!(
            TargetSpec::parse("@1-3").unwrap(),
            TargetSpec::Index(vec![1, 2, 3])
        );
        assert_eq!(
            TargetSpec::parse("@1,3").unwrap(),
            TargetSpec::Index(vec![1, 3])
        );
        assert!(TargetSpec::parse("@3-1").is_err());
        assert!(TargetSpec::parse("@x").is_err());
    }

    #[test]
    fn index_zero_is_rejected() {
        // 序号是 1-based：`@0` / `@0-3` 在语法层就该被挡下，不能带着 0 进到 daemon。
        for raw in ["@0", "@0-3", "@0,2"] {
            let err = TargetSpec::parse(raw).unwrap_err();
            assert_eq!(err.code, "invalid_target_spec", "{raw}");
            assert!(err.message.contains('0'), "{raw}: {}", err.message);
            assert!(!err.hint.is_empty(), "{raw}");
        }
    }

    #[test]
    fn huge_index_ranges_do_not_overflow() {
        // `usize::MAX` 参与区间运算：`end - start + 1` 会整数溢出。
        // debug 下这里原本直接 panic（exit 101），必须改成稳定的 SpecError。
        let max = usize::MAX;
        for raw in [
            format!("@0-{max}"),
            format!("@1-{max}"),
            "@1-MAX".to_string(),
        ] {
            let err = TargetSpec::parse(&raw).unwrap_err();
            assert_eq!(err.code, "invalid_target_spec", "{raw}");
            assert!(!err.hint.is_empty(), "{raw}");
        }
        // 单点 usize::MAX 语法合法（真的越界由 daemon 报「没有第 N 个标签页」）。
        assert!(TargetSpec::parse(&format!("@{max}")).is_ok());
        // 合法区间的上限仍然生效，边界值 512 放行、513 拒绝。
        assert!(TargetSpec::parse("@1-512").is_ok());
        assert!(TargetSpec::parse("@1-513").is_err());
    }

    #[test]
    fn session_key_is_a_valid_id() {
        let key = "browser-a:profile-b:123";
        let spec = TargetSpec::parse(key).unwrap();
        assert_eq!(spec.single_id(), Some(key));
    }

    #[test]
    fn whitespace_inside_id_is_rejected() {
        let err = TargetSpec::parse("111 222").unwrap_err();
        assert_eq!(err.code, "invalid_target_spec");
        assert!(!err.hint.is_empty());
    }
}
