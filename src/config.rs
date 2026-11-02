use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_EXTENSION_PORT: u16 = 18765;
pub const CLI_API_PORT: u16 = 18767;
pub const API_TOKEN_HEADER: &str = "x-agent-browser-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub extension_port: u16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            extension_port: DEFAULT_EXTENSION_PORT,
        }
    }
}

pub fn load_or_create() -> Result<AppConfig> {
    let path = config_path()?;
    let content = match read_with_backup(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let config = AppConfig::default();
            save(&config)?;
            return Ok(config);
        }
        Err(err) => {
            return Err(err).with_context(|| format!("读取配置文件失败: {}", path.display()))
        }
    };
    if content.trim().is_empty() {
        let config = AppConfig::default();
        save(&config)?;
        return Ok(config);
    }

    let config: AppConfig = serde_json::from_str(&content)
        .with_context(|| format!("解析配置文件失败: {}", path.display()))?;
    validate_port(config.extension_port)?;
    Ok(config)
}

pub fn load_existing() -> Result<AppConfig> {
    let path = config_path()?;
    let content =
        read_with_backup(&path).with_context(|| format!("读取配置文件失败: {}", path.display()))?;
    if content.trim().is_empty() {
        return Err(anyhow!("配置文件为空: {}", path.display()));
    }
    let config: AppConfig = serde_json::from_str(&content)
        .with_context(|| format!("解析配置文件失败: {}", path.display()))?;
    validate_port(config.extension_port)?;
    Ok(config)
}

pub fn save(config: &AppConfig) -> Result<()> {
    validate_port(config.extension_port)?;
    let path = config_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("配置文件路径缺少父目录: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "创建配置目录失败（请检查该目录的写权限）: {}",
            parent.display()
        )
    })?;
    let content = serde_json::to_string_pretty(config)? + "\n";
    // 与 token 一样走「临时文件 + 原子替换」，避免并发读方读到写了一半的 JSON。
    let temp_path = parent.join(format!(".config-{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&temp_path)
            .with_context(|| format!("创建配置文件临时文件失败: {}", temp_path.display()))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        replace_file_atomically(&temp_path, &path)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub fn set_extension_port(port: u16) -> Result<AppConfig> {
    validate_port(port)?;
    let mut config = load_or_create()?;
    config.extension_port = port;
    save(&config)?;
    Ok(config)
}

pub fn config_path() -> Result<PathBuf> {
    Ok(user_config_dir()?.join("config.json"))
}

pub fn user_config_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("无法定位用户主目录，不能创建 agent-browser-cli 配置文件"))?;
    Ok(PathBuf::from(home).join(".agent-browser-cli"))
}

pub fn log_dir() -> Result<PathBuf> {
    Ok(user_config_dir()?.join("logs"))
}

pub fn daemon_log_path() -> Result<PathBuf> {
    Ok(log_dir()?.join("daemon.log"))
}

pub fn api_token_path() -> Result<PathBuf> {
    Ok(user_config_dir()?.join("api-token"))
}

/// `net` 规则（driver.net_hooks）的落盘路径，与 ws-trust.json 同目录。
///
/// 结构：`{ "<browser:profile>": <net 配置 Value> }`。daemon 重启后读回，
/// 避免「页面里的 hook 还在、`net routes` 却读到空」的配置漂移。
pub fn net_hooks_path() -> Result<PathBuf> {
    Ok(user_config_dir()?.join("net-hooks.json"))
}

/// 读取全部 net 规则；文件不存在视为空表。
pub fn load_net_hooks() -> Result<HashMap<String, serde_json::Value>> {
    load_net_hooks_from(&net_hooks_path()?)
}

fn load_net_hooks_from(path: &Path) -> Result<HashMap<String, serde_json::Value>> {
    let content = match read_with_backup(path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("读取 net-hooks 失败: {}", path.display()))
        }
    };
    if content.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let map: HashMap<String, serde_json::Value> = serde_json::from_str(&content)
        .with_context(|| format!("解析 net-hooks 失败: {}", path.display()))?;
    Ok(map)
}

/// 覆盖写入全部 net 规则（临时文件 + 原子替换，权限 0600）。
pub fn save_net_hooks(map: &HashMap<String, serde_json::Value>) -> Result<()> {
    save_net_hooks_to(&net_hooks_path()?, map)
}

fn save_net_hooks_to(path: &Path, map: &HashMap<String, serde_json::Value>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("net-hooks 路径缺少父目录: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "创建配置目录失败（请检查该目录的写权限）: {}",
            parent.display()
        )
    })?;
    let content = serde_json::to_string_pretty(map)? + "\n";
    let temp_path = parent.join(format!(".net-hooks-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temp_path)
            .with_context(|| format!("创建 net-hooks 临时文件失败: {}", temp_path.display()))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        // Windows 上打开中的文件无法被 rename，先显式关闭句柄。
        drop(file);
        replace_file_atomically(&temp_path, path)
            .with_context(|| format!("写入 net-hooks 失败: {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub fn load_api_token() -> Result<Option<String>> {
    let path = api_token_path()?;
    let content = match read_with_backup(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("读取 API token 失败: {}", path.display()))
        }
    };
    let token = content.trim();
    if token.is_empty() {
        // 空 token 文件等价于「尚未生成 token」，不要让整条命令直接失败。
        return Ok(None);
    }
    Ok(Some(token.to_string()))
}

pub fn save_api_token(token: &str) -> Result<()> {
    if token.is_empty() {
        return Err(anyhow!("API token 不能为空"));
    }
    let path = api_token_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("API token 路径缺少父目录: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "创建配置目录失败（请检查该目录的写权限）: {}",
            parent.display()
        )
    })?;
    let temp_path = parent.join(format!(".api-token-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temp_path)
            .with_context(|| format!("创建 API token 临时文件失败: {}", temp_path.display()))?;
        file.write_all(token.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        // Windows 上打开中的文件无法被 rename，先显式关闭句柄。
        drop(file);
        replace_file_atomically(&temp_path, &path)
            .with_context(|| format!("保存 API token 失败: {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub fn ensure_log_file() -> Result<PathBuf> {
    let path = daemon_log_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建日志目录失败: {}", parent.display()))?;
    }
    if !path.exists() {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("创建日志文件失败: {}", path.display()))?;
    }
    Ok(path)
}

/// 原子替换时使用的备份路径（同目录，`api-token` → `api-token.bak`）。
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".bak");
    path.with_file_name(name)
}

/// 读取文件；目标不存在时回退读 `.bak`。
///
/// Windows 上的原子替换是「备份 → 替换 → 删备份」三步，若进程在替换与删备份之间退出，
/// 只有 `.bak` 里还留着旧内容。回退读保证崩溃/并发之后 token 与配置不会凭空消失。
///
/// 公开给 `server.rs`：`ws-trust.json` 的读取也必须走这条路径，否则「目标暂时不存在、
/// 备份里还有旧配置」会被当成「从没配过」，安全白名单静默退化成兼容模式（fail-open）。
pub fn read_with_backup(path: &Path) -> std::io::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        // 只在「目标不存在」时回退，权限等其他错误照旧上抛，避免掩盖真实问题。
        Err(err) if err.kind() == ErrorKind::NotFound => fs::read_to_string(backup_path(path)),
        Err(err) => Err(err),
    }
}

/// 用已经写好的临时文件替换目标文件，避免目标出现「短暂不存在」的窗口期。
///
/// 调用方需保证临时文件与目标在**同一目录**（跨盘 rename 会失败），并已把内容
/// `sync_all` 落盘。`ws-trust.json` 等信任相关配置也走这里，避免「先删后改名」
/// 在 Windows 上把安全配置删成「未配置」而 fail-open。
pub fn replace_file_atomically(temp_path: &Path, target: &Path) -> std::io::Result<()> {
    // Unix: rename 直接覆盖目标，本身就原子。
    #[cfg(not(windows))]
    {
        fs::rename(temp_path, target)
    }
    // Windows: rename 不会覆盖已存在的目标，所以先备份、再替换、成功后删备份；
    // 替换失败时把备份放回去，保证目标文件不会永久缺失。
    #[cfg(windows)]
    {
        let backup = backup_path(target);
        let had_target = target.exists();
        if had_target {
            let _ = fs::remove_file(&backup);
            fs::rename(target, &backup)?;
        }
        if let Err(err) = fs::rename(temp_path, target) {
            if had_target {
                let _ = fs::rename(&backup, target);
            }
            return Err(err);
        }
        if had_target {
            let _ = fs::remove_file(&backup);
        }
        Ok(())
    }
}

fn validate_port(port: u16) -> Result<()> {
    if port == 0 {
        return Err(anyhow!("extension_port 必须是 1-65535"));
    }
    if port == CLI_API_PORT {
        return Err(anyhow!(
            "18767 是 agent-browser-cli API 端口，插件端口请换一个"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = env::temp_dir().join(format!(
            "abc-config-test-{}-{}-{}",
            tag,
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn net_hooks_store_load_roundtrip() {
        let dir = temp_dir("net-hooks-roundtrip");
        let path = dir.join("net-hooks.json");
        let mut map = HashMap::new();
        map.insert(
            "chrome:profile-1".to_string(),
            serde_json::json!({
                "persist": true,
                "routes": [{ "match": "/api" }],
                "cdpEnabled": true,
                "cdpRules": [],
            }),
        );

        save_net_hooks_to(&path, &map).expect("save net hooks");
        let loaded = load_net_hooks_from(&path).expect("load net hooks");
        assert_eq!(loaded, map);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn net_hooks_reset_clears_persisted_entry() {
        let dir = temp_dir("net-hooks-reset");
        let path = dir.join("net-hooks.json");
        let mut map = HashMap::new();
        map.insert(
            "chrome:profile-1".to_string(),
            serde_json::json!({ "persist": true }),
        );
        map.insert(
            "chrome:profile-2".to_string(),
            serde_json::json!({ "persist": false }),
        );
        save_net_hooks_to(&path, &map).expect("seed");

        // reset 语义：删掉目标 profile 后整体覆盖写回，其它 profile 必须保留。
        map.remove("chrome:profile-1");
        save_net_hooks_to(&path, &map).expect("save after reset");
        let loaded = load_net_hooks_from(&path).expect("load after reset");
        assert_eq!(loaded.len(), 1);
        assert!(!loaded.contains_key("chrome:profile-1"));
        assert!(loaded.contains_key("chrome:profile-2"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn net_hooks_broken_file_does_not_panic() {
        let dir = temp_dir("net-hooks-broken");
        let path = dir.join("net-hooks.json");
        fs::write(&path, "{ not json").expect("seed broken file");

        let err = load_net_hooks_from(&path).expect_err("broken file must be an error");
        assert!(err.to_string().contains("解析"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replace_file_leaves_new_content_and_no_backup() {
        let dir = temp_dir("replace");
        let target = dir.join("api-token");
        fs::write(&target, "old\n").expect("seed target");
        let temp = dir.join("api-token.tmp");
        fs::write(&temp, "new\n").expect("seed temp");

        replace_file_atomically(&temp, &target).expect("replace");
        assert_eq!(fs::read_to_string(&target).expect("read target"), "new\n");
        assert!(!backup_path(&target).exists());
        assert!(!temp.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_falls_back_to_backup_only_when_target_missing() {
        let dir = temp_dir("backup");
        let target = dir.join("api-token");
        fs::write(backup_path(&target), "old\n").expect("seed backup");

        assert_eq!(read_with_backup(&target).expect("backup read"), "old\n");
        fs::write(&target, "new\n").expect("seed target");
        assert_eq!(read_with_backup(&target).expect("target read"), "new\n");

        fs::remove_file(&target).expect("remove target");
        fs::remove_file(backup_path(&target)).expect("remove backup");
        assert_eq!(
            read_with_backup(&target).expect_err("missing").kind(),
            ErrorKind::NotFound
        );

        fs::remove_dir_all(&dir).ok();
    }
}
