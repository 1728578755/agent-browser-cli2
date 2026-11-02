use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, serialize, ParseOpts};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};

pub fn js_opt_html() -> &'static str {
    include_str!("../assets/simphtml_opt.js")
}

/// 页面结构化读取（标题/正文/元信息/表单/链接）。
pub fn js_page_read() -> &'static str {
    include_str!("../assets/page_read.js")
}

/// 元素查找（CSS / 文本 / XPath → 可操作信息）。
pub fn js_page_find() -> &'static str {
    include_str!("../assets/page_find.js")
}

/// 单往返动作（hover / click / select / check / scroll / setValue / focus）。
pub fn js_page_action() -> &'static str {
    include_str!("../assets/page_action.js")
}

/// DOM 版快照（CDP 不可用时的回退），产出带 CSS 选择器的 @e 列表。
pub fn js_page_snapshot() -> &'static str {
    include_str!("../assets/page_snapshot.js")
}

/// 页面网络：fetch / hook / log / route / reset。
pub fn js_page_net() -> &'static str {
    include_str!("../assets/page_net.js")
}

/// 页面内等待（MutationObserver，一次注入完成）。
pub fn js_page_wait() -> &'static str {
    include_str!("../assets/page_wait.js")
}

/// 挑战页 / 验证码识别（只识别与等待，不自动破解）。
pub fn js_page_challenge() -> &'static str {
    include_str!("../assets/page_challenge.js")
}

pub fn optimize_html_for_tokens(html: &str) -> String {
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(html.to_string());
    clean_node(&dom.document);
    let mut bytes = Vec::new();
    let serializable = SerializableHandle::from(dom.document.clone());
    if serialize(&mut bytes, &serializable, Default::default()).is_ok() {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        html.to_string()
    }
}

/// DOM 递归下钻的深度上限。
///
/// 取 512 的理由：
/// - 与浏览器 HTML 解析器的实际嵌套上限同量级，正常页面（含深层组件库）远到不了这个深度；
/// - 超过之后 `clean_node` 和后面的 html5ever 序列化都会递归，留着才是栈溢出风险；
/// - 命中阈值时只丢弃该节点的子树，不改变其余输出，正常页面的产物与之前逐字节一致。
const MAX_DOM_DEPTH: usize = 512;

fn clean_node(handle: &Handle) {
    clean_node_at(handle, 0);
}

fn clean_node_at(handle: &Handle, depth: usize) {
    if depth >= MAX_DOM_DEPTH {
        // 极端嵌套（恶意/生成式 DOM）：停止下钻，保留该层节点本身但丢弃子树，避免进程 panic。
        handle.children.borrow_mut().clear();
        return;
    }
    if let NodeData::Element { name, attrs, .. } = &handle.data {
        let tag = name.local.as_ref();
        let mut attrs = attrs.borrow_mut();
        if tag.eq_ignore_ascii_case("svg") {
            // 之前整棵 svg 的属性和子树都被清空，图标按钮的可读语义（aria-label / role /
            // 内联 <title> / data-testid）全丢。现在只保留可读属性 + 可能带文案的
            // <title> 子节点，绘制路径（path/circle/g/…）照旧丢弃，token 预算不受影响。
            attrs.retain(|attr| {
                let key = attr.name.local.as_ref();
                matches!(key, "aria-label" | "aria-hidden" | "role" | "title")
                    || key.starts_with("data-test")
            });
            for attr in attrs.iter_mut() {
                if attr.value.len() > 100 {
                    let short: String = attr.value.chars().take(50).collect();
                    attr.value.clear();
                    attr.value.push_slice(&format!("{} ...", short));
                }
            }
            drop(attrs);
            let mut children = handle.children.borrow_mut();
            children.retain(|child| {
                matches!(&child.data, NodeData::Element { name, .. }
                    if name.local.as_ref().eq_ignore_ascii_case("title"))
            });
            for child in children.iter() {
                if let NodeData::Text { contents } = &child.data {
                    let mut text = contents.borrow_mut();
                    if text.len() > 120 {
                        let short: String = text.chars().take(80).collect();
                        text.clear();
                        text.push_slice(&short);
                    }
                }
            }
            return;
        }
        attrs.retain(|attr| {
            let key = attr.name.local.as_ref();
            matches!(
                key,
                "id" | "class"
                    | "name"
                    | "src"
                    | "href"
                    | "alt"
                    | "value"
                    | "type"
                    | "placeholder"
                    | "disabled"
                    | "checked"
                    | "selected"
                    | "readonly"
                    | "required"
                    | "multiple"
                    | "role"
                    | "aria-label"
                    | "aria-expanded"
                    | "aria-hidden"
                    | "contenteditable"
                    | "title"
                    | "for"
                    | "action"
                    | "method"
                    | "target"
                    | "colspan"
                    | "rowspan"
            ) || key.starts_with("data-")
        });
        for attr in attrs.iter_mut() {
            let key = attr.name.local.as_ref();
            let value = attr.value.to_string();
            if key == "src" {
                if value.starts_with("data:") {
                    attr.value.clear();
                    attr.value.push_slice("__img__");
                } else if value.len() > 30 {
                    attr.value.clear();
                    attr.value.push_slice("__url__");
                }
            } else if (key == "href" || key == "action") && value.len() > 30 {
                attr.value.clear();
                attr.value
                    .push_slice(if key == "href" { "__link__" } else { "__url__" });
            } else if matches!(key, "value" | "title" | "alt") && value.len() > 100 {
                attr.value.clear();
                attr.value.push_slice(&format!(
                    "{} ...",
                    value.chars().take(50).collect::<String>()
                ));
            } else if key.starts_with("data-") && !key.starts_with("data-v") && value.len() > 20 {
                attr.value.clear();
                attr.value.push_slice("__data__");
            }
        }
    }
    let children = handle.children.borrow().clone();
    for child in children {
        clean_node_at(&child, depth + 1);
    }
}

pub fn smart_truncate(html: String, budget: usize) -> String {
    // budget 是「字符」预算（下游按 token/可见字数设计），而 String::len() 是 UTF-8 字节数。
    // 用字节数比较会让中文页面提前约 3 倍截断，这里统一按 chars().count() 计算。
    // 调用方可能仍然用字节长度做前置判断，所以「其实没超预算」时必须原样返回。
    if html.chars().count() <= budget {
        return html;
    }
    let keep = budget.saturating_sub(32);
    let mut result: String = html.chars().take(keep).collect();
    result.push_str(" [TRUNCATED]");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_truncate_counts_chars_not_bytes() {
        // 300 字节但只有 100 个字符：预算 100 时不该被截断。
        let html = "汉".repeat(100);
        assert!(!smart_truncate(html.clone(), 100).contains("[TRUNCATED]"));
        let truncated = smart_truncate(html, 50);
        assert!(truncated.ends_with(" [TRUNCATED]"));
        // 保留 budget - 32 个字符 + 标记本身。
        let keep = 50 - 32;
        assert_eq!(
            truncated.chars().count(),
            keep + " [TRUNCATED]".chars().count()
        );
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        // 远超深度上限的嵌套：不 panic，超出部分被丢弃、浅层内容保留。
        let depth = 10_000;
        let html = format!(
            "<p>deep</p>{}x{}",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        );
        let out = optimize_html_for_tokens(&html);
        assert!(out.contains("deep"));
        assert!(out.matches("<div>").count() <= MAX_DOM_DEPTH + 2);
    }

    #[test]
    fn svg_keeps_readable_labels() {
        let html = r#"<button><svg aria-label="设置" role="img" data-testid="gear" viewBox="0 0 24 24" fill="none"><title>齿轮</title><path d="M0 0h24v24H0z"/></svg></button>"#;
        let out = optimize_html_for_tokens(html);
        assert!(out.contains("aria-label=\"设置\""), "{out}");
        assert!(out.contains("data-testid=\"gear\""), "{out}");
        assert!(out.contains("<title>齿轮</title>"), "{out}");
        // 绘制路径仍要丢掉，避免 token 浪费。
        assert!(!out.contains("<path"), "{out}");
        assert!(!out.contains("viewBox"), "{out}");
    }
}
