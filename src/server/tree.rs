use super::*;

/// 树形 JSON 递归下钻的深度上限。
///
/// 正常页面/无障碍树的深度远小于这个值（实测都在 50 层以内）；限制只为防御
/// 极端嵌套（生成式或恶意 DOM）导致的栈溢出：命中阈值时保留当前层节点、丢弃子树。
pub(crate) const MAX_TREE_DEPTH: usize = 256;

/// 只保留「自身有 ref」或「子孙里有 ref」的节点，把纯结构性容器折叠掉。
pub(crate) fn prune_tree_keep_refs(nodes: Vec<Value>) -> Vec<Value> {
    prune_tree_keep_refs_at(nodes, 0)
}

pub(crate) fn prune_tree_keep_refs_at(nodes: Vec<Value>, depth: usize) -> Vec<Value> {
    let mut kept = Vec::new();
    for mut node in nodes {
        // 用 take 把子树「搬」出来，不要 `children.clone()`：深层子树 clone 本身就是
        // 一次无上限递归拷贝，2000 层就能把栈打爆，等于绕过了下面的深度保护。
        let children = match node.get_mut("children").and_then(Value::as_array_mut) {
            Some(array) => std::mem::take(array),
            None => Vec::new(),
        };
        // 超深：不再下钻（丢弃子树），当前层节点照常参与 keep 判断。
        let kept_children = if depth >= MAX_TREE_DEPTH || children.is_empty() {
            Vec::new()
        } else {
            prune_tree_keep_refs_at(children, depth + 1)
        };
        let has_ref = node.get("ref").is_some();
        if !kept_children.is_empty() {
            if let Some(object) = node.as_object_mut() {
                object.insert("children".to_string(), json!(kept_children));
            }
        } else if let Some(object) = node.as_object_mut() {
            object.remove("children");
        }
        if has_ref || node.get("children").is_some() {
            kept.push(node);
        }
    }
    kept
}

/// 按深度优先顺序截断到 `max_nodes` 个节点（0 表示不限制）。
pub(crate) fn fold_tree(nodes: Vec<Value>, max_nodes: usize) -> Vec<Value> {
    if max_nodes == 0 {
        return nodes;
    }
    let mut budget = max_nodes;
    fold_tree_inner(nodes, &mut budget, 0)
}

pub(crate) fn fold_tree_inner(nodes: Vec<Value>, budget: &mut usize, depth: usize) -> Vec<Value> {
    let mut kept = Vec::new();
    for mut node in nodes {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        // 同 prune：take 而不是 clone，避免深层子树递归拷贝绕过深度上限。
        let children = match node.get_mut("children").and_then(Value::as_array_mut) {
            Some(array) => std::mem::take(array),
            None => Vec::new(),
        };
        if !children.is_empty() {
            if depth >= MAX_TREE_DEPTH {
                // 超深：保留节点本身，丢掉子树。
                if let Some(object) = node.as_object_mut() {
                    object.remove("children");
                }
            } else {
                let kept_children = fold_tree_inner(children, budget, depth + 1);
                if let Some(object) = node.as_object_mut() {
                    if kept_children.is_empty() {
                        object.remove("children");
                    } else {
                        object.insert("children".to_string(), json!(kept_children));
                    }
                }
            }
        }
        kept.push(node);
    }
    kept
}

/// 把超过上限的返回值换成「截断预览」，避免一条命令把上下文塞满。
///
/// 上限按**字符**计（与 `--max-chars` 同口径）。历史字段 `--max-bytes` 名字容易
/// 误解成字节，实际一直都是字符；这里把函数名和判断都统一到字符语义。
///
/// 预览取的是**值本身**的内容，而不是序列化后的文本：旧实现直接截序列化文本的前 N 个
/// 字符，开头那个 `"` 与内部的 `\"` 转义都算进预算，`max_chars=1` 时 preview 就是 `"`，
/// 等于什么都没有（P3）。
pub(crate) fn truncate_json_value_chars(result: &mut Value, max_chars: usize) -> bool {
    let Some(slot) = result.get_mut("js_return") else {
        return false;
    };
    // 字符串取原值（不含外层引号），其它类型取紧凑 JSON 文本。
    let text = match &*slot {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    };
    // UTF-8 里字节数恒 >= 字符数：字节数都没超就必然没超，先做 O(1) 快速放行，
    // 避免为了判断一次截断而遍历整个字符串两遍。
    if text.len() <= max_chars {
        return false;
    }
    let total = text.chars().count();
    if total <= max_chars {
        return false;
    }
    let preview: String = text.chars().take(max_chars).collect();
    *slot = json!({
        "__truncated": true,
        "total_chars": total,
        "preview": preview,
    });
    true
}

/// 统计树里带 `ref` 的节点数（深度上限与 fold_tree 一致）。
///
/// snapshot 的 `returned` 必须和「实际交给调用方的 @e 数量」同口径：
/// CDP 路径以前统计的是快照缓存里的 refs（截断前），JS 路径统计的是树节点数，
/// 两条路在同一份数据上会给出不同的数（P3）。
pub(crate) fn count_tree_refs(nodes: &[Value]) -> usize {
    count_tree_refs_at(nodes, 0)
}

fn count_tree_refs_at(nodes: &[Value], depth: usize) -> usize {
    let mut count = 0;
    for node in nodes {
        if node.get("ref").is_some() {
            count += 1;
        }
        if depth >= MAX_TREE_DEPTH {
            continue;
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            count += count_tree_refs_at(children, depth + 1);
        }
    }
    count
}

/// 把 `execute_page_js` 的信封拆开：页面返回值放到 `key` 下，保留定位信息。
pub(crate) fn page_payload(mut result: Value, key: &str) -> Value {
    let payload = result
        .get_mut("js_return")
        .map(|value| value.take())
        .unwrap_or(Value::Null);
    json!({
        "tab_id": result.get("tab_id").cloned().unwrap_or(Value::Null),
        "session_key": result.get("session_key").cloned().unwrap_or(Value::Null),
        key: payload,
    })
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    /// P3：preview 截的是「值本身」，不能再把外层引号与转义算进预算。
    #[test]
    fn truncation_preview_keeps_real_text() {
        let mut value = json!({ "js_return": "hello" });
        assert!(truncate_json_value_chars(&mut value, 1));
        assert_eq!(value["js_return"]["preview"], json!("h"));
        assert_eq!(value["js_return"]["total_chars"], json!(5));

        // 没超预算时原样返回，不做任何包装。
        let mut small = json!({ "js_return": "hi" });
        assert!(!truncate_json_value_chars(&mut small, 10));
        assert_eq!(small["js_return"], json!("hi"));

        // 中文字符按字符计，不按字节。
        let mut chinese = json!({ "js_return": "汉字内容" });
        assert!(truncate_json_value_chars(&mut chinese, 2));
        assert_eq!(chinese["js_return"]["preview"], json!("汉字"));
    }
}
