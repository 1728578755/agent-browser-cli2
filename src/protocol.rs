use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub browser_id: String,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_label: Option<String>,
    #[serde(default)]
    pub session_key: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "type", default = "default_tab_type")]
    pub tab_type: String,
    #[serde(default)]
    pub connected_at: Option<f64>,
}

fn default_tab_type() -> String {
    "ext_ws".to_string()
}

#[derive(Debug, Clone)]
pub struct Session {
    pub session_key: String,
    pub tab_id: String,
    pub browser_id: String,
    pub profile_id: String,
    pub profile_label: Option<String>,
    pub extension_version: Option<String>,
    pub file_scheme_access: Option<bool>,
    pub info: TabInfo,
    pub sender: mpsc::UnboundedSender<String>,
    pub disconnected_at: Option<Instant>,
}

impl Session {
    pub fn is_active(&self) -> bool {
        self.disconnected_at.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// 保留字段（当前恒为 `None`，无写入点）。
    ///
    /// 扩展回包只有 `data` / `newTabs` 两个真实载荷，`result` / `closed` 是早期
    /// 设计的遗留：读取点都写成 `data.or(result)`，属于历史上的空转。读取点分散在
    /// 6 处，删除属于纯清理（不改语义），为避免一次性扩大改动面暂留，标注备用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 保留字段（当前恒为 `None`，无写入点），见 `result`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "newTabs")]
    pub new_tabs: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RectInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementDomInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rect: Option<RectInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementRef {
    pub ref_id: String,
    /// AX 快照的来源：CDP backendNodeId；JS 快照时为 0。
    pub backend_dom_node_id: i64,
    /// JS 快照产生的 CSS 选择器；有值时 `@e` 动作可完全走页面内执行，不依赖 CDP。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    pub index: usize,
    pub role: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom: Option<ElementDomInfo>,
}

#[derive(Debug, Clone)]
pub struct SnapshotCache {
    pub generation: u64,
    pub url: String,
    pub refs: HashMap<String, ElementRef>,
}

#[derive(Debug)]
pub struct PendingExec {
    pub delivered_at: Option<Instant>,
    pub tx: oneshot::Sender<anyhow::Result<ExecResult>>,
}

#[derive(Default)]
pub struct DriverState {
    pub sessions: HashMap<String, Session>,
    pub snapshots: HashMap<String, SnapshotCache>,
    pub default_session_key: Option<String>,
    pub latest_session_key: Option<String>,
    pub preferred_default_session_key: Option<String>,
    /// 每个 browser:profile 的 net 配置（含页面规则与网络层规则），跨标签共享。
    pub net_hooks: HashMap<String, Value>,
}

/// 在途执行登记表：从 DriverState 拆出来单独一把锁。
///
/// 每个 exec 结果、每个 ACK 都要动这张表。以前它们和 `sessions` 共用一把全局
/// Mutex，等于所有命令的结果回传都要抢同一把锁；拆开后结果回传只碰这张小表。
#[derive(Default)]
pub struct ExecRegistry {
    pub pending: HashMap<String, PendingExec>,
    pub acked: HashSet<String>,
    /// exec_id -> session_key：扩展断开/标签关闭时按会话批量失败。
    pub active: HashMap<String, String>,
}

impl ExecRegistry {
    pub fn register(&mut self, exec_id: &str, session_key: &str, pending: PendingExec) {
        self.pending.insert(exec_id.to_string(), pending);
        self.active
            .insert(exec_id.to_string(), session_key.to_string());
    }

    /// 取走一个执行并清干净：结果、错误、超时、断开都走这里。
    pub fn take(&mut self, exec_id: &str) -> Option<PendingExec> {
        self.acked.remove(exec_id);
        self.active.remove(exec_id);
        self.pending.remove(exec_id)
    }

    /// 某个会话断开/关闭时，把它名下的在途执行全部取走。
    pub fn take_session(&mut self, session_key: &str) -> Vec<PendingExec> {
        let ids: Vec<String> = self
            .active
            .iter()
            .filter(|(_, key)| key.as_str() == session_key)
            .map(|(id, _)| id.clone())
            .collect();
        ids.iter().filter_map(|id| self.take(id)).collect()
    }

    /// 清理已失效会话名下的登记（会话被 prune 掉时用）。
    pub fn retain_sessions(&mut self, alive: &[String]) {
        let dead: Vec<String> = self
            .active
            .iter()
            .filter(|(_, key)| !alive.iter().any(|item| item == *key))
            .map(|(id, _)| id.clone())
            .collect();
        for id in dead {
            self.take(&id);
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum WsIncoming {
    #[serde(rename = "ext_ready")]
    ExtReady {
        #[serde(default = "default_browser_id")]
        browser_id: String,
        #[serde(default = "default_profile_id")]
        profile_id: String,
        #[serde(default)]
        profile_label: Option<String>,
        #[serde(default)]
        extension_version: Option<String>,
        #[serde(default)]
        file_scheme_access: Option<bool>,
        tabs: Vec<ExtTab>,
    },
    #[serde(rename = "tabs_update")]
    TabsUpdate {
        #[serde(default = "default_browser_id")]
        browser_id: String,
        #[serde(default = "default_profile_id")]
        profile_id: String,
        #[serde(default)]
        profile_label: Option<String>,
        #[serde(default)]
        extension_version: Option<String>,
        #[serde(default)]
        file_scheme_access: Option<bool>,
        tabs: Vec<ExtTab>,
    },
    #[serde(rename = "ack")]
    Ack { id: String },
    #[serde(rename = "result")]
    Result {
        id: String,
        result: Value,
        #[serde(rename = "newTabs")]
        new_tabs: Option<Value>,
    },
    #[serde(rename = "error")]
    Error {
        id: String,
        error: Value,
        #[serde(rename = "newTabs")]
        new_tabs: Option<Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct ExtTab {
    pub id: Value,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
}

impl ExtTab {
    pub fn into_tab_info(
        self,
        browser_id: &str,
        profile_id: &str,
        profile_label: Option<String>,
    ) -> TabInfo {
        let tab_id = match self.id {
            Value::String(s) => s,
            other => other.to_string(),
        };
        let session_key = make_session_key(browser_id, profile_id, &tab_id);
        TabInfo {
            id: tab_id.clone(),
            tab_id,
            browser_id: browser_id.to_string(),
            profile_id: profile_id.to_string(),
            profile_label,
            session_key,
            url: self.url,
            title: self.title,
            tab_type: "ext_ws".to_string(),
            connected_at: None,
        }
    }
}

pub fn make_session_key(browser_id: &str, profile_id: &str, tab_id: &str) -> String {
    format!("{browser_id}:{profile_id}:{tab_id}")
}

fn default_browser_id() -> String {
    "browser-default".to_string()
}

fn default_profile_id() -> String {
    "profile-default".to_string()
}
