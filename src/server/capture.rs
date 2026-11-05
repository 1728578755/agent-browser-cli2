use super::*;

pub(crate) async fn screenshot(
    State(state): State<AppState>,
    Json(req): Json<ScreenshotRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(match screenshot_page(&state, req).await {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn save_pdf(
    State(state): State<AppState>,
    Json(req): Json<SavePdfRequest>,
) -> Json<Value> {
    touch(&state).await;
    Json(match save_pdf_page(&state, req).await {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    })
}

pub(crate) async fn screenshot_page(state: &AppState, req: ScreenshotRequest) -> Result<Value> {
    let tab_id = select_tab(
        state,
        // 选择器字段按值传走，后面的回落分支还要读完整个 req（格式/质量/输出路径），
        // 这里 clone 三个短字符串比把参数拆成一长串更划算。
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
    )
    .await?;
    let timeout = secs(req.timeout.max(0.1));
    let format = normalize_screenshot_format(&req.format)?;
    let target = match (&req.target, &req.selector) {
        (Some(_), Some(_)) => {
            return Err(anyhow!("screenshot: --target 不能和 --selector 同时使用"))
        }
        (Some(target), None) | (None, Some(target)) => Some(target.clone()),
        (None, None) => None,
    };
    // 非接触路径：可见区域截屏用 chrome.tabs.captureVisibleTab，不 attach 调试器。
    // 只有「目标标签页是当前激活页 + 不需要整页/元素裁剪」时才适用，
    // 其余情况回落到 CDP（整页、元素、后台标签页都必须 CDP）。
    let can_capture_visible = target.is_none() && !req.full_page;
    let visible_eligible = can_capture_visible && tab_is_active(state, &tab_id).await;
    let mut capture_visible_error: Option<String> = None;
    if visible_eligible {
        // 非调试器通道失败的原因要留下来：页面被别的扩展 iframe 挡住时 CDP
        // 一定失败，此时「为什么连 captureVisibleTab 也没成」才是真正的线索。
        match capture_visible_once(
            state,
            &tab_id,
            format,
            req.quality,
            req.out.as_deref(),
            timeout,
        )
        .await
        {
            Ok(value) => return Ok(value),
            Err(err) => capture_visible_error = Some(humanize_extension_error(&err.to_string())),
        }
    }

    // 整页 / 元素 / 后台标签页都必须走调试器；页面被别的扩展 iframe 挡住时
    // CDP 一定失败，此时先把失败原因留下来，再决定是回落还是直接报错。
    let cdp_error =
        match capture_via_cdp(state, &tab_id, target.as_deref(), &req, format, timeout).await {
            Ok(value) => return Ok(value),
            Err(err) => err,
        };
    if !is_debugger_blocked(&cdp_error.to_string()) {
        // 参数错 / 超时 / NO_DEBUGGER 之类回落也无意义，保持原文。
        return Err(cdp_error);
    }
    // 首次 captureVisibleTab 很可能是撞上了 Chrome 的配额/限流（每秒调用次数
    // 有限），而 debugger_blocked 恰恰是最需要这条通道的时候。退避后再试一次，
    // 两次都失败才把两个原因一起交给调用方。
    if visible_eligible {
        // 先 clone 出首次原因：接着要整体覆盖 `capture_visible_error`，不能一边借用一边赋值。
        let first_error = capture_visible_error.clone();
        if let Some(first) = first_error {
            tokio::time::sleep(CAPTURE_VISIBLE_RETRY_BACKOFF).await;
            match capture_visible_once(
                state,
                &tab_id,
                format,
                req.quality,
                req.out.as_deref(),
                timeout,
            )
            .await
            {
                Ok(value) => return Ok(value),
                Err(err) => {
                    capture_visible_error = Some(format!(
                        "{first}；退避 {}ms 重试仍失败：{}",
                        CAPTURE_VISIBLE_RETRY_BACKOFF.as_millis(),
                        humanize_extension_error(&err.to_string())
                    ));
                }
            }
        }
    }
    let visible_note = match capture_visible_error {
        Some(reason) => format!("；非调试器截图（captureVisibleTab）同样不可用：{reason}"),
        None => String::new(),
    };
    if req.full_page && target.is_none() {
        // 只有「纯整页」能回落：扩展侧的 captureFullPage 用 captureVisibleTab +
        // OffscreenCanvas 拼接，元素截图/后台标签页没有对应能力，直接报错。
        // （--target 配 --full-page 时 CDP 截的是元素框，换成整页会让语义悄悄变掉。）
        return match capture_full_page_stitched(
            state,
            &tab_id,
            format,
            req.quality,
            req.max_height_px,
            req.out.as_deref(),
            timeout,
        )
        .await
        {
            Ok(value) => Ok(value),
            Err(stitch_error) => {
                // 诊断要等两件事都失败才做：成功回落时不该为一句「关哪个扩展」
                // 多等 2s。
                let blocking = blocking_extensions_hint(state, &tab_id).await;
                Err(anyhow!(
                    "{}；整页拼图回落（captureFullPage）同样失败：{stitch_error}{visible_note}",
                    add_screenshot_protected_hint(cdp_error, &blocking)
                ))
            }
        };
    }
    let blocking = blocking_extensions_hint(state, &tab_id).await;
    Err(anyhow!(
        "{}{visible_note}",
        add_screenshot_protected_hint(cdp_error, &blocking)
    ))
}

/// 首次 `captureVisibleTab` 失败后的退避时长（落在 150–300ms 区间）。
const CAPTURE_VISIBLE_RETRY_BACKOFF: Duration = Duration::from_millis(200);

/// 目标标签页是不是浏览器当前激活页（决定能不能走非调试器通道）。
async fn tab_is_active(state: &AppState, tab_id: &str) -> bool {
    let driver = state.driver.read().await;
    driver
        .sessions
        .get(tab_id)
        .map(|session| session.info.active)
        .unwrap_or(false)
}

/// 一次 `chrome.tabs.captureVisibleTab` 截图（已解码 + 校验 + 落盘）。
///
/// 抽成函数是为了「退避重试」：首次失败与重试必须走完全相同的
/// 解码/校验/落盘路径，否则重试成功也可能写出一张坏图。
async fn capture_visible_once(
    state: &AppState,
    tab_id: &str,
    format: &str,
    quality: Option<u8>,
    out: Option<&Path>,
    timeout: Duration,
) -> Result<Value> {
    let payload = json!({ "format": format, "quality": quality });
    let value = extension_control(state, tab_id, "captureVisible", payload, timeout).await?;
    // `extension_control` 已经把扩展回包的 `data` 层剥掉了，这里拿到的就是
    // `{data_url, format, via}`；老形状（`data.data_url`）一并兼容。
    let data_url = data_url_of(&value)
        .ok_or_else(|| anyhow!("captureVisible 未返回 data_url（扩展版本可能不支持该通道）"))?;
    let base64 = data_url.rsplit(',').next().unwrap_or_default();
    guard_encoded_capture_len(base64.len())?;
    let bytes = decode_base64(base64)?;
    validate_screenshot_bytes(&bytes, format)?;
    let path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_screenshot_path(format));
    write_bytes(&path, &bytes)?;
    Ok(json!({
        "status": "success",
        "path": path,
        "format": format,
        "bytes": bytes.len(),
        "target": Value::Null,
        "full_page": false,
        "via": "captureVisibleTab"
    }))
}

/// CDP 截图路径：构造参数（整页要先量内容尺寸）→ `Page.captureScreenshot` → 校验 → 落盘。
///
/// 失败一律**原样**返回：调用方要拿原始文案判断能不能回落，提示文案统一在外层拼，
/// 免得「已加过提示的错误」再被加一遍。
async fn capture_via_cdp(
    state: &AppState,
    tab_id: &str,
    target: Option<&str>,
    req: &ScreenshotRequest,
    format: &str,
    timeout: Duration,
) -> Result<Value> {
    let mut params = json!({ "format": format });
    if format == "jpeg" {
        let quality = req.quality.unwrap_or(80);
        if !(1..=100).contains(&quality) {
            return Err(anyhow!("screenshot: quality 必须在 1..=100"));
        }
        params["quality"] = json!(quality);
    }
    let mut truncated = false;
    let mut requested_height: Option<u32> = None;
    if let Some(target) = target {
        let rect = target_rect(state, tab_id, target, timeout).await?;
        params["clip"] = json!({
            "x": rect.x,
            "y": rect.y,
            "width": rect.width,
            "height": rect.height,
            "scale": 1.0
        });
    } else if req.full_page {
        let metrics = cdp_call(state, tab_id, "Page.getLayoutMetrics", json!({}), timeout).await?;
        if !metrics.ok {
            return Err(anyhow!(metrics.error.unwrap_or_else(|| {
                "screenshot: Page.getLayoutMetrics failed".to_string()
            })));
        }
        let metrics_data = metrics.data.unwrap_or(Value::Null);
        let content_size = metrics_data
            .get("contentSize")
            .cloned()
            .ok_or_else(|| anyhow!("screenshot: missing contentSize"))?;
        // `--max-height` 以前只在扩展拼图通道生效，CDP 主路径直接忽略。
        // 这里按 `max_height_px / dpr` 截断 clip（clip 单位是 CSS px），并如实标注
        // `truncated`，否则调用方以为拿到了完整长图。
        let dpr = metrics_dpr(&metrics_data);
        let mut clip_height = content_size
            .get("height")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if let Some(max_px) = req.max_height_px.filter(|value| *value > 0) {
            let max_css = f64::from(max_px) / dpr;
            if clip_height > max_css {
                clip_height = max_css;
                truncated = true;
                requested_height = Some(max_px);
            }
        }
        params["clip"] = json!({
            "x": content_size.get("x").and_then(Value::as_f64).unwrap_or(0.0),
            "y": content_size.get("y").and_then(Value::as_f64).unwrap_or(0.0),
            "width": content_size.get("width").and_then(Value::as_f64).unwrap_or(0.0),
            "height": clip_height,
            "scale": 1.0
        });
        params["captureBeyondViewport"] = json!(true);
    }
    let captured = cdp_call(state, tab_id, "Page.captureScreenshot", params, timeout).await?;
    if !captured.ok {
        return Err(anyhow!(captured.error.unwrap_or_else(|| {
            "screenshot: Page.captureScreenshot failed".to_string()
        })));
    }
    let data = captured
        .data
        .and_then(|data| data.get("data").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| anyhow!("screenshot: CDP response missing data"))?;
    guard_encoded_capture_len(data.len())?;
    let bytes = decode_base64(&data)?;
    // 解码成功不代表数据可用：空/截断的 base64 会得到 0 字节或半截文件。
    validate_screenshot_bytes(&bytes, format)?;
    let path = req
        .out
        .clone()
        .unwrap_or_else(|| default_screenshot_path(format));
    write_bytes(&path, &bytes)?;
    let mut body = json!({
        "status": "success",
        "path": path,
        "format": format,
        "bytes": bytes.len(),
        "target": target,
        "full_page": req.full_page,
        "via": "cdp"
    });
    if req.full_page {
        body["truncated"] = json!(truncated);
        if let Some(requested) = requested_height {
            body["requested_height"] = json!(requested);
            body["hint"] = json!(format!(
                "已按 --max-height {requested}px（设备像素）截断，超出部分未包含在截图里"
            ));
        }
    }
    Ok(body)
}

/// `Page.getLayoutMetrics` 回包里的设备像素比：用 `contentSize / cssContentSize`
/// 的宽度比例反推（旧扩展可能不返回 `cssContentSize`，此时按 1.0 处理）。
fn metrics_dpr(data: &Value) -> f64 {
    let device_width = data
        .get("contentSize")
        .and_then(|size| size.get("width"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let css_width = data
        .get("cssContentSize")
        .and_then(|size| size.get("width"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if device_width > 0.0 && css_width > 0.0 {
        let ratio = device_width / css_width;
        if ratio.is_finite() {
            return ratio.clamp(0.1, 8.0);
        }
    }
    1.0
}

/// `captureFullPage` 的产物：拼接后的整页图 + 尺寸/切片信息。
struct StitchedShot {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    slices: u64,
    truncated: bool,
    dpr: f64,
    /// 回包尺寸与 JPEG 真实像素不一致、已按图像修正。
    size_corrected: bool,
    /// JPEG 尺寸解析失败时的提示（保留回包值，但如实告知无法交叉验证）。
    size_warning: Option<String>,
}

/// 走扩展的「整页拼图」通道（captureVisibleTab + OffscreenCanvas，不 attach 调试器）。
async fn fetch_full_page(
    state: &AppState,
    tab_id: &str,
    format: &str,
    quality: Option<u8>,
    max_height_px: Option<u32>,
    timeout: Duration,
) -> Result<StitchedShot> {
    let mut payload = json!({ "format": format });
    if let Some(quality) = quality {
        payload["quality"] = json!(quality);
    }
    // 语义是「设备像素总高」（与 scrollH*dpr 比较），不要再乘/除 dpr。
    if let Some(height) = max_height_px {
        payload["maxHeightPx"] = json!(height);
    }
    let value = extension_control(state, tab_id, "captureFullPage", payload, timeout).await?;
    // `extension_control` 已经剥掉扩展回包的 `data` 层；老形状（`data.data_url`）一并兼容。
    let data_url = data_url_of(&value)
        .ok_or_else(|| anyhow!("captureFullPage 未返回 data_url（扩展版本可能不支持整页拼图）"))?;
    let base64 = data_url.rsplit(',').next().unwrap_or_default();
    // 整页拼接的 base64 可以到几百 MB，先按字符串长度准入，
    // 超限直接失败，绝不先解码到内存再判断（那一步已经 OOM 了）。
    guard_encoded_capture_len(base64.len())?;
    let bytes = decode_base64(base64)?;
    validate_screenshot_bytes(&bytes, format)?;
    let mut width = pixel_dim(&value, "width")?;
    let mut height = pixel_dim(&value, "height")?;
    // 回包里的 width/height 来自扩展的记账，可能和真正拼出来的位图不一致。
    // JPEG 的 SOF 段里有权威像素尺寸，以图像为准，并在返回体标注已修正。
    let mut size_corrected = false;
    let mut size_warning: Option<String> = None;
    if format == "jpeg" {
        match jpeg_dimensions(&bytes) {
            Some((real_width, real_height)) => {
                if real_width != width || real_height != height {
                    width = real_width;
                    height = real_height;
                    size_corrected = true;
                }
            }
            // 解析不了就沿用回包值，但要明说「没能交叉验证」，不能装作校验过。
            None => {
                size_warning =
                    Some("无法从 JPEG 的 SOF 段解析真实像素尺寸，已沿用扩展回包值".to_string())
            }
        }
    }
    // 拼接结果的像素总量也有预算，否则单张图就能吃掉几百 MB。
    let max_pixels = crate::settings::env_usize_clamped(
        "ATB_CAPTURE_MAX_PIXELS",
        80_000_000,
        1_000_000,
        500_000_000,
    ) as u64;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > max_pixels {
        return Err(anyhow!(
            "capture_too_large: 整页图 {width}x{height}（{pixels} 像素）超过上限 {max_pixels} 像素；\
             可用 --max-height 限制高度，或调高环境变量 ATB_CAPTURE_MAX_PIXELS"
        ));
    }
    Ok(StitchedShot {
        width,
        height,
        slices: value.get("slices").and_then(Value::as_u64).unwrap_or(0),
        truncated: value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        dpr: value.get("dpr").and_then(Value::as_f64).unwrap_or(1.0),
        size_corrected,
        size_warning,
        bytes,
    })
}

/// 整页拼图写入文件，返回体如实标注「这是拼出来的」以及吸顶元素的重复问题。
async fn capture_full_page_stitched(
    state: &AppState,
    tab_id: &str,
    format: &str,
    quality: Option<u8>,
    max_height_px: Option<u32>,
    out: Option<&Path>,
    timeout: Duration,
) -> Result<Value> {
    let shot = fetch_full_page(state, tab_id, format, quality, max_height_px, timeout).await?;
    let path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_screenshot_path(format));
    write_bytes(&path, &shot.bytes)?;
    let mut body = json!({
        "status": "success",
        "path": path,
        "format": format,
        "bytes": shot.bytes.len(),
        "target": Value::Null,
        "full_page": true,
        "via": "captureFullPageStitched",
        "stitched": true,
        "slices": shot.slices,
        "truncated": shot.truncated,
        // 吸顶/吸底元素在每条切片里都会出现一次，拼接消不掉，必须如实告知。
        "sticky_artifacts": "possible",
        "dpr": shot.dpr,
        // 回包尺寸与 JPEG 真实像素不一致时以图像为准，这里标注已修正。
        "size_corrected": shot.size_corrected,
        "width": shot.width,
        "height": shot.height
    });
    if let Some(warning) = &shot.size_warning {
        body["size_warning"] = json!(warning);
    }
    Ok(body)
}

/// base64 字符串长度准入。
///
/// 解码前先按「base64 长度 ≈ 字节数 × 4/3」估算，超过输出上限就直接失败。
/// 先解码再判断没有意义：到了那一步内存已经被吃掉。
fn guard_encoded_capture_len(encoded_len: usize) -> Result<()> {
    let cap = crate::settings::io_max_output_bytes();
    let max_encoded = cap.saturating_mul(4) / 3 + 4;
    if encoded_len > max_encoded {
        return Err(anyhow!(
            "capture_too_large: 画面数据 base64 {} 字节（约 {} 字节原始数据）超过输出上限 {} 字节；\
             可用 --max-height 缩小截图范围，或调高环境变量 ATB_IO_MAX_OUTPUT_MB",
            encoded_len,
            encoded_len.saturating_mul(3) / 4,
            cap
        ));
    }
    Ok(())
}

/// 手写 JPEG SOF 解析：从 SOF0/SOF2 等帧头读出真实宽高。
///
/// 只需要宽度和高度，不引入解码器。解析失败返回 `None`（调用方保留回包值并给 warning）。
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut index = 2usize;
    while index + 1 < bytes.len() {
        // 段必须以 0xFF 开头；中间夹了填充字节时跳过。
        if bytes[index] != 0xff {
            index += 1;
            continue;
        }
        let mut marker_index = index + 1;
        while marker_index < bytes.len() && bytes[marker_index] == 0xff {
            marker_index += 1;
        }
        if marker_index >= bytes.len() {
            return None;
        }
        let marker = bytes[marker_index];
        // 独立标记（RST0..RST7 / SOI / EOI / TEM）没有长度字段。
        if (0xd0..=0xd9).contains(&marker) || marker == 0x01 {
            index = marker_index + 1;
            continue;
        }
        let length_index = marker_index + 1;
        if length_index + 1 >= bytes.len() {
            return None;
        }
        let segment_len = usize::from(u16::from_be_bytes([
            bytes[length_index],
            bytes[length_index + 1],
        ]));
        if segment_len < 2 {
            return None;
        }
        // SOF0..SOF15，排除 DHT(0xC4)、JPG(0xC8)、DAC(0xCC) 这三个非帧头标记。
        let is_sof = matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        );
        if is_sof {
            // 载荷：precision(1) height(2) width(2) ...
            let payload = length_index + 2;
            if payload + 5 > bytes.len() {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([bytes[payload + 1], bytes[payload + 2]]));
            let width = u32::from(u16::from_be_bytes([bytes[payload + 3], bytes[payload + 4]]));
            if width == 0 || height == 0 {
                return None;
            }
            return Some((width, height));
        }
        index = length_index + segment_len;
    }
    None
}

/// `extension_control` 回包里的 `data_url`（已剥层或老形状）。
fn data_url_of(value: &Value) -> Option<&str> {
    value.get("data_url").and_then(Value::as_str).or_else(|| {
        value
            .get("data")
            .and_then(|data| data.get("data_url"))
            .and_then(Value::as_str)
    })
}

/// 回包里的像素尺寸：缺失或非正数都没法算页面大小，直接报错。
fn pixel_dim(value: &Value, key: &str) -> Result<u32> {
    let dim = value.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    if !dim.is_finite() || dim < 1.0 {
        return Err(anyhow!("captureFullPage 未返回有效尺寸（{key}={dim}）"));
    }
    Ok(dim.round().min(f64::from(u32::MAX)) as u32)
}

/// 诊断命令的硬超时：它只是给错误文案补一句「关掉哪个扩展」，
/// 绝不能拖慢主错误路径，所以比正常命令的超时小一个数量级。
const BLOCKING_DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(2);

/// 「到底是哪个扩展在挡路」。
///
/// Chrome 只回一句「cannot access a chrome-extension:// url of different extension」，
/// 用户无从知道该关哪个扩展；扩展侧能读到页面 iframe 的扩展 id/名字，这里把它拼进
/// 错误文案。诊断本身失败（旧扩展、超时、页面已关）一律静默跳过。
async fn blocking_extensions_hint(state: &AppState, tab_id: &str) -> String {
    let response = tokio::time::timeout(
        BLOCKING_DIAGNOSTIC_TIMEOUT,
        extension_control(
            state,
            tab_id,
            "blockingExtensions",
            json!({}),
            BLOCKING_DIAGNOSTIC_TIMEOUT,
        ),
    )
    .await;
    let Ok(Ok(value)) = response else {
        return String::new();
    };
    let Some(blocking) = value.get("blocking").and_then(Value::as_array).or_else(|| {
        value
            .get("data")
            .and_then(|data| data.get("blocking"))
            .and_then(Value::as_array)
    }) else {
        return String::new();
    };
    let mut names: Vec<String> = Vec::new();
    for item in blocking {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .or_else(|| item.get("id").and_then(Value::as_str));
        let Some(name) = name else { continue };
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        return String::new();
    }
    format!(
        "；检测到页面内嵌了扩展「{}」的 iframe，Chrome 拒绝 attach；可临时禁用该扩展后重试",
        names.join("、")
    )
}

/// 「页面禁止调试器注入」这类失败的统一判据：CDP 英文原文与 daemon 翻译后的中文
/// 文案都认。只有这一类才值得回落（其它错误回落也一样会失败）。
fn is_debugger_blocked(message: &str) -> bool {
    is_page_protected_error(message) || message.contains("禁止调试器注入")
}

pub(crate) async fn save_pdf_page(state: &AppState, req: SavePdfRequest) -> Result<Value> {
    let tab_id = select_tab(
        state,
        // 选择器字段按值传走，后面的回落分支还要读完整个 req（输出路径/纸张/缩放），
        // 这里 clone 三个短字符串比把参数拆成一长串更划算。
        SessionSelector::new(
            req.switch_tab_id.clone(),
            req.browser.clone(),
            req.profile.clone(),
        ),
    )
    .await?;
    let timeout = secs(req.timeout.max(0.1));
    let (paper_width, paper_height) = paper_size(&req.paper)?;
    if !(0.1..=2.0).contains(&req.scale) {
        return Err(anyhow!("save-pdf: scale 必须在 0.1..=2.0"));
    }
    // 先用会话缓存里的标题兜底（零往返）。以前是先 `document.title`
    // 再打印，受限页面会为此白等一个完整的 CDP 往返，最后又把它吞成 "page"。
    let cached_title = cached_page_title(state, &tab_id).await;
    let printed = cdp_call(
        state,
        &tab_id,
        "Page.printToPDF",
        json!({
            "printBackground": req.print_background,
            "landscape": req.landscape,
            "scale": req.scale,
            "paperWidth": paper_width,
            "paperHeight": paper_height,
            "preferCSSPageSize": true
        }),
        timeout,
    )
    .await;
    // 失败有两条路径（`Err` 与 `ok:false`），「页面禁止调试器注入」都可能落在上面，
    // 先合并判定，再去走图片型 PDF 回落。
    let debugger_blocked = match &printed {
        Err(err) => is_debugger_blocked(&err.to_string()),
        Ok(result) if !result.ok => result
            .error
            .as_deref()
            .map(is_debugger_blocked)
            .unwrap_or(false),
        _ => false,
    };
    if debugger_blocked {
        // 受限页面：标题只能用缓存值，不再为它等一个注定失败的往返。
        let title = match cached_title {
            Some(value) => TitleOutcome {
                value,
                fallback: false,
                error: None,
            },
            None => TitleOutcome {
                value: "page".to_string(),
                fallback: true,
                error: Some(
                    "该页面禁止调试器注入，无法读取 document.title，且会话缓存无标题".to_string(),
                ),
            },
        };
        return save_pdf_stitched(state, &tab_id, &req, &title, timeout).await;
    }
    let result = printed?;
    if !result.ok {
        // 非受限错误：原文直出，别替调用方猜原因。
        return Err(anyhow!(result
            .error
            .unwrap_or_else(|| "save-pdf: Page.printToPDF failed".to_string())));
    }
    let data = result
        .data
        .and_then(|data| data.get("data").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| anyhow!("save-pdf: CDP response missing data"))?;
    guard_encoded_capture_len(data.len())?;
    let bytes = decode_base64(&data)?;
    // 同 screenshot：先校验魔数与长度，再考虑输出上限。
    validate_pdf_bytes(&bytes)?;
    let max_pdf_bytes = crate::settings::io_max_output_bytes();
    if bytes.len() > max_pdf_bytes {
        return Err(anyhow!(
            "save-pdf: PDF {} 字节超过输出上限 {max_pdf_bytes} 字节（ATB_IO_MAX_OUTPUT_MB）",
            bytes.len()
        ));
    }
    // 打印成功说明 CDP 通道可用，此时再取实时标题（失败就用缓存值并如实标注）。
    let title = resolve_page_title(state, &tab_id, timeout, cached_title).await;
    let path = req
        .out
        .clone()
        .unwrap_or_else(|| default_pdf_path(&title.value));
    write_bytes(&path, &bytes)?;
    let mut body = json!({
        "status": "success",
        "path": path,
        "bytes": bytes.len(),
        "paper": req.paper.to_ascii_lowercase(),
        "landscape": req.landscape,
        "scale": req.scale,
        "print_background": req.print_background,
        "title": title.value
    });
    if title.fallback {
        body["title_fallback"] = json!(true);
        if let Some(reason) = title.error {
            body["title_error"] = json!(reason);
        }
    }
    Ok(body)
}

/// `document.title` 的取值结果 + 是否用了兜底。
struct TitleOutcome {
    value: String,
    fallback: bool,
    error: Option<String>,
}

/// 会话缓存里的标题（扩展注册时上报），零往返。
async fn cached_page_title(state: &AppState, tab_id: &str) -> Option<String> {
    let driver = state.driver.read().await;
    driver
        .sessions
        .get(tab_id)
        .map(|session| session.info.title.trim().to_string())
        .filter(|title| !title.is_empty())
}

/// 取实时标题；失败时退回缓存值，并把原因带出去（不再静默变成 "page"）。
async fn resolve_page_title(
    state: &AppState,
    tab_id: &str,
    timeout: Duration,
    cached: Option<String>,
) -> TitleOutcome {
    match current_title(state, tab_id, timeout).await {
        Ok(value) if !value.trim().is_empty() => TitleOutcome {
            value,
            fallback: false,
            error: None,
        },
        Ok(_) => TitleOutcome {
            value: cached.unwrap_or_else(|| "page".to_string()),
            fallback: true,
            error: Some("document.title 为空，已回落到会话缓存标题".to_string()),
        },
        Err(err) => TitleOutcome {
            value: cached.unwrap_or_else(|| "page".to_string()),
            fallback: true,
            error: Some(err.to_string()),
        },
    }
}

/// 图片型 PDF 回落：整页拼图 → JPEG 包成（可能多页的）PDF。
///
/// 页面禁止调试器注入时 `Page.printToPDF` 不可用，但用户要的是一份**能看的**整页
/// 文档；文字不可选中这点必须在返回体里说清楚，否则调用方会以为拿到了可检索 PDF。
async fn save_pdf_stitched(
    state: &AppState,
    tab_id: &str,
    req: &SavePdfRequest,
    title: &TitleOutcome,
    timeout: Duration,
) -> Result<Value> {
    // 只用 JPEG：DCTDecode 能直接吃原始字节，省掉一次重编码。
    let shot = match fetch_full_page(state, tab_id, "jpeg", Some(85), req.max_height_px, timeout)
        .await
    {
        Ok(shot) => shot,
        Err(err) => {
            let blocking = blocking_extensions_hint(state, tab_id).await;
            return Err(anyhow!(
                "save-pdf 需要调试器注入，但该页面禁止调试器注入（页面内嵌了其他扩展的 iframe）。\n\
                 图片型 PDF 回落（captureFullPage）同样失败：{err}{blocking}\n\
                 替代方案：改用 atb screenshot --full-page 截长图；或临时关闭注入 iframe 的扩展后重试"
            ));
        }
    };
    // 拼图回包的 width/height 是**设备像素**，PDF 的页面尺寸必须按
    // CSS px（96dpi）换算。dpr=2 时不归一化会把页面放大一倍。
    let dpr = if shot.dpr.is_finite() && shot.dpr > 0.0 {
        shot.dpr
    } else {
        1.0
    };
    let width_px = normalize_css_dimension(shot.width, dpr);
    let height_px = normalize_css_dimension(shot.height, dpr);
    // 超长页面在 image_pdf 内部纵向切片，页高不会超过 PDF 上限。
    let pdf = pdf::image_pdf(&shot.bytes, width_px, height_px)?;
    let bytes = pdf.bytes;
    // 自产的 PDF 也过同一套校验：`%PDF-` 头 + `%%EOF` 尾，拼接错位时不会静默成功。
    validate_pdf_bytes(&bytes)?;
    let max_pdf_bytes = crate::settings::io_max_output_bytes();
    if bytes.len() > max_pdf_bytes {
        return Err(anyhow!(
            "save-pdf: PDF {} 字节超过输出上限 {max_pdf_bytes} 字节（ATB_IO_MAX_OUTPUT_MB）",
            bytes.len()
        ));
    }
    let path = req
        .out
        .clone()
        .unwrap_or_else(|| default_pdf_path(&title.value));
    write_bytes(&path, &bytes)?;
    // 这是图片型回退，请求里的 paper/landscape/scale **没有生效**。
    // 回显必须如实：`paper:"image"`、`landscape/scale:null`，并给出实际页面几何。
    let mut body = json!({
        "status": "success",
        "path": path,
        "bytes": bytes.len(),
        "paper": "image",
        "landscape": Value::Null,
        "scale": Value::Null,
        "paper_applied": false,
        "requested_paper": req.paper.to_ascii_lowercase(),
        "requested_landscape": req.landscape,
        "requested_scale": req.scale,
        "print_background": req.print_background,
        "via": "stitched-image-pdf",
        "text_selectable": false,
        "truncated": shot.truncated,
        "pages": pdf.pages,
        "width_pt": round2(pdf.width_pt),
        "height_pt": round2(pdf.height_pt),
        "page_height_pt": round2(pdf.page_height_pt),
        "dpr": dpr,
        "size_corrected": shot.size_corrected,
        "title": title.value.clone(),
        "note": "该页面禁止调试器注入，PDF 为整页截图合成（文字不可选中）；\
                 请求的 paper/landscape/scale 未生效，页面尺寸由截图决定"
    });
    if title.fallback {
        body["title_fallback"] = json!(true);
        if let Some(reason) = &title.error {
            body["title_error"] = json!(reason);
        }
    }
    if let Some(warning) = &shot.size_warning {
        body["size_warning"] = json!(warning);
    }
    Ok(body)
}

/// 设备像素 → CSS px（保留 1px 下限，dpr 非法时按 1.0 处理）。
fn normalize_css_dimension(device_px: u32, dpr: f64) -> u32 {
    let value = (f64::from(device_px) / dpr).round();
    value.clamp(1.0, f64::from(u32::MAX)) as u32
}

/// 回显用的两位小数（避免 JSON 里出现 960.0000000000001 这类噪声）。
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(crate) async fn target_rect(
    state: &AppState,
    tab_id: &str,
    target: &str,
    timeout: Duration,
) -> Result<RectInfo> {
    let object_id = resolve_target_object(state, tab_id, target, timeout).await?;
    let script = "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); const r = this.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; }";
    let value = call_function_resolved(state, tab_id, &object_id, script, timeout).await?;
    let rect: RectInfo = serde_json::from_value(value)?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return Err(anyhow!("screenshot: target has no layout box"));
    }
    Ok(rect)
}

/// 读取 `document.title`。空值或取值失败一律返回 `Err`，由调用方决定兜底，
/// 不再静默变成 "page"。
pub(crate) async fn current_title(
    state: &AppState,
    tab_id: &str,
    timeout: Duration,
) -> Result<String> {
    let response = execute_raw_js_on_tab(state, "return document.title", tab_id, timeout).await?;
    let value = response.data.unwrap_or(Value::Null);
    match value.as_str() {
        Some(title) if !title.trim().is_empty() => Ok(title.to_string()),
        Some(_) => Err(anyhow!("document.title 为空")),
        None => Err(anyhow!("document.title 取值失败（回包不是字符串）")),
    }
}

pub(crate) fn normalize_screenshot_format(format: &str) -> Result<&'static str> {
    match format.to_ascii_lowercase().as_str() {
        "png" => Ok("png"),
        "jpg" | "jpeg" => Ok("jpeg"),
        other => Err(anyhow!(
            "screenshot: unsupported format {other}, expected png or jpeg"
        )),
    }
}

pub(crate) fn paper_size(paper: &str) -> Result<(f64, f64)> {
    match paper.to_ascii_lowercase().as_str() {
        "letter" => Ok((8.5, 11.0)),
        "legal" => Ok((8.5, 14.0)),
        "a4" => Ok((8.27, 11.69)),
        "a3" => Ok((11.69, 16.54)),
        "tabloid" => Ok((11.0, 17.0)),
        other => Err(anyhow!("save-pdf: unsupported paper {other}")),
    }
}

pub(crate) fn default_screenshot_path(format: &str) -> PathBuf {
    // 不要硬编码 /tmp：Windows 上会落到当前盘符根目录（可能是只读或意外位置）。
    std::env::temp_dir().join("atb-screenshots").join(format!(
        "screenshot-{}.{}",
        timestamp_compact(),
        if format == "jpeg" { "jpg" } else { format }
    ))
}

pub(crate) fn default_pdf_path(title: &str) -> PathBuf {
    std::env::temp_dir().join("atb-pdfs").join(format!(
        "{}-{}.pdf",
        sanitize_filename(title),
        timestamp_compact()
    ))
}

pub(crate) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| {
            // 带上绝对路径：只回一句 "No such file or directory" 时调用方无法判断
            // 是哪个目录没权限/没建起来（P3）。
            anyhow!(
                "写入失败：无法创建输出目录 {}：{err}",
                absolute_path(parent).display()
            )
        })?;
    }
    fs::write(path, bytes).map_err(|err| {
        // 错误信息必须能定位：绝对路径 + 父目录是否存在。
        // 以前只有底层 errno 文本，调用方无从知道是路径写错还是目录根本不存在。
        let parent_state = match path.parent() {
            Some(parent) if parent.exists() => "存在",
            Some(_) => "不存在",
            None => "无",
        };
        anyhow!(
            "写入失败：{}（父目录{parent_state}）：{err}",
            absolute_path(path).display()
        )
    })?;
    Ok(())
}

/// 把（可能是相对的）输出路径补成绝对路径，只用于错误信息。
fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|dir| dir.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// 文件名里的时间戳：毫秒精度 + 4 位随机后缀。
///
/// 秒级时间戳在同秒内连拍（脚本批量截图 / 循环存 PDF）会互相覆盖，
/// 而且 `write_bytes` 是直接覆盖写，用户只会看到最后一张。
pub(crate) fn timestamp_compact() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis();
    let salt = Uuid::new_v4().simple().to_string();
    format!("{millis}-{}", &salt[..4])
}

pub(crate) fn sanitize_filename(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        let mapped = if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            || ch.is_control()
            || ch.is_whitespace()
        {
            '-'
        } else {
            ch
        };
        if mapped == '-' && out.ends_with('-') {
            continue;
        }
        out.push(mapped);
        if out.chars().count() >= 80 {
            break;
        }
    }
    // 只裁尾部：`trim_matches('-')` 会把开头的 `-` 也削掉，`-draft` 这类合法文件名
    // 首字符被吃掉后可能与另一个文件同名、互相覆盖（P3）。
    let trimmed = out.trim_end_matches(['-', '_', ' ']).to_string();
    if trimmed.is_empty() {
        "page".to_string()
    } else {
        trimmed
    }
}

pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>> {
    // 除了「能解出来」，还要能判断「这份数据是不是完整的」（D-02）：
    // ① `=` 之后不允许再有数据字符（拼接/截断的典型形态）；
    // ② 带 padding 时总长度必须是 4 的倍数；
    // ③ 最后一组残留 bit 必须为 0（末字节只来了半个字符时不为 0）。
    // 这三条只能挡住「非 4 对齐」的截断；恰好截掉 4/8 个字符的情况
    // 由 `validate_screenshot_bytes` 的**尾部魔数**兜底 —— 那才是真正的证据。
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    let mut padding = 0usize;
    let mut symbols = 0usize;
    for byte in input.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if byte == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err(anyhow!(
                "base64 数据在 `=` 之后还有内容（共 {padding} 个 `=`），说明串被拼接或截断过"
            ));
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(anyhow!("invalid base64 data")),
        } as u32;
        symbols += 1;
        buf = (buf << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    if padding > 2 {
        return Err(anyhow!("base64 末尾 `=` 超过 2 个，数据非法"));
    }
    if padding > 0 && !(symbols + padding).is_multiple_of(4) {
        return Err(anyhow!(
            "base64 长度不是 4 的倍数（{symbols} 个数据字符 + {padding} 个 `=`），数据被截断"
        ));
    }
    if padding == 0 && symbols % 4 == 1 {
        return Err(anyhow!("base64 数据只剩 1 个字符的尾巴，数据被截断"));
    }
    if bits > 0 && (buf & ((1u32 << bits) - 1)) != 0 {
        return Err(anyhow!("base64 末尾残留 bit 非零，数据被截断"));
    }
    Ok(out)
}

/// 截图数据的最低可信长度。比一张 1×1 的 PNG（约 70 字节）略大：
/// 真实视口截图不可能比它更短，短于这个值只可能是空/截断传输。
pub(crate) const MIN_SCREENSHOT_BYTES: usize = 100;
/// PDF 的最低可信长度。
pub(crate) const MIN_PDF_BYTES: usize = 200;

/// 校验 CDP 回传的画面数据：**能解码 ≠ 可用**。
///
/// `decode_base64` 对空串和截断串都会「成功」返回（0 字节或半截文件），
/// 旧实现照样写盘并返回 `status:success`，退出码 0，调用方拿到坏图却以为成功（C-10）。
pub(crate) fn validate_screenshot_bytes(bytes: &[u8], format: &str) -> Result<()> {
    let magic_ok = match format {
        "png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        // 其它格式不认魔数，只校验长度（当前 CLI 只产出 png/jpeg）。
        _ => true,
    };
    if bytes.len() < MIN_SCREENSHOT_BYTES || !magic_ok {
        return Err(anyhow!(
            "浏览器返回的画面数据无效（{format}，{} 字节，魔数{}），未写入文件",
            bytes.len(),
            if magic_ok { "正确" } else { "不符" }
        ));
    }
    // 尾部魔数：只查头部的话，「截掉尾部若干个 4 字符组」的 base64 会被判成合法，
    // 于是写盘一张打不开的图、却报 status:success（D-02，v7 已实测复现）。
    // 只在最后 16 字节里找，容忍少量尾部填充，但不接受「完全没有结束标记」。
    if !tail_magic_ok(bytes, format) {
        return Err(anyhow!(
            "浏览器返回的画面数据不完整（{format}，{} 字节，缺少结尾标记）；\
             通常是传输被截断，未写入文件（原实现会写出一张打不开的图并报成功）",
            bytes.len()
        ));
    }
    Ok(())
}

/// 结尾标记：PNG 的 `IEND` 块（`49 45 4e 44 ae 42 60 82`）、JPEG 的 `FF D9`。
fn tail_magic_ok(bytes: &[u8], format: &str) -> bool {
    const IEND: [u8; 8] = [0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];
    let tail_start = bytes.len().saturating_sub(16);
    let tail = &bytes[tail_start..];
    match format {
        "png" => tail.windows(IEND.len()).any(|window| window == IEND),
        "jpeg" => tail.windows(2).any(|window| window == [0xff, 0xd9]),
        _ => true,
    }
}

pub(crate) fn validate_pdf_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.len() < MIN_PDF_BYTES || !bytes.starts_with(b"%PDF-") {
        return Err(anyhow!(
            "浏览器返回的画面数据无效（PDF，{} 字节），未写入文件",
            bytes.len()
        ));
    }
    // 与截图同理：PDF 必须以 `%%EOF` 收尾，只有 `%PDF-` 头说明文件被截断（D-02 同族）。
    let tail_start = bytes.len().saturating_sub(64);
    if !bytes[tail_start..]
        .windows(5)
        .any(|window| window == b"%%EOF")
    {
        return Err(anyhow!(
            "浏览器返回的 PDF 不完整（{} 字节，缺少结尾的 %%EOF），未写入文件",
            bytes.len()
        ));
    }
    Ok(())
}

/// 整页/元素截图必须走调试器；页面被别的扩展 iframe 挡住时 Chrome 拒绝 attach。
/// 这里把「还能怎么截」写进错误，避免调用方对着 CDP 原文猜。
/// `blocking` 是 `blocking_extensions_hint` 的结论（可能为空串）：能说清是哪个扩展
/// 在挡路时，用户才真的能「临时禁用后重试」。
fn add_screenshot_protected_hint(err: anyhow::Error, blocking: &str) -> anyhow::Error {
    if is_debugger_blocked(&err.to_string()) {
        return anyhow!(
            "{err}；整页与元素截图必须使用调试器。可去掉 --full-page / --selector 改截可见区域（走非调试器通道），或临时关闭注入 iframe 的扩展后重试{blocking}"
        );
    }
    err
}
