// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 不坑老师 · https://github.com/44886/Exeify
// Copyright 2026 zjqzte2010 · https://github.com/zjqzte2010/web2exe（二次开发）

//! GUI 打包器模式：白色极简界面 + 通过 IPC 驱动打包。

use crate::config::{SplashCfg, WindowCfg, WindowState};
use crate::packer;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{http::Request, WebViewBuilder};

/// 事件循环自定义事件
enum UserEvent {
    /// 来自前端的原始 IPC 消息
    Ipc(String),
    /// 在 WebView 中执行 JS
    Eval(String),
}

/// 前端发来的消息
#[derive(Debug, Deserialize)]
struct IpcMsg {
    action: String,
    #[serde(default)]
    data: Option<PackReq>,
    #[serde(default, rename = "defaultName")]
    default_name: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// 打包请求
#[derive(Debug, Deserialize)]
struct PackReq {
    mode: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    title: String,
    width: f64,
    height: f64,
    resizable: bool,
    output: String,
    /// 启动状态："normal" | "maximized" | "fullscreen"
    #[serde(default)]
    state: Option<String>,
    /// 启动图文件路径（.png/.jpg），空则不生成启动页
    #[serde(default)]
    splash: Option<String>,
    /// 启动页最少显示毫秒
    #[serde(default)]
    splash_ms: Option<u64>,
    /// 启动页背景色 "#rrggbb"
    #[serde(default)]
    splash_bg: Option<String>,
    /// 是否同时生成安卓 APK
    #[serde(default)]
    make_apk: Option<bool>,
    /// APK 输出路径（为空时按 exe 同名推导）
    #[serde(default)]
    apk: Option<String>,
}

/// 把图标文件读成 data URL，用于界面预览。文件过大则返回 None。
fn icon_data_url(path: &std::path::Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.len() > 4 * 1024 * 1024 {
        return None;
    }
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    };
    Some(format!(
        "data:{};base64,{}",
        mime,
        crate::b64::encode(&data)
    ))
}

/// 把启动图文件读成 data URL（用于界面预览）。仅接受 png/jpg，过大返回 None。
fn splash_data_url(path: &std::path::Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.len() > 8 * 1024 * 1024 {
        return None;
    }
    let mime = splash_mime(path)?;
    Some(format!(
        "data:{};base64,{}",
        mime,
        crate::b64::encode(&data)
    ))
}

/// 由扩展名判断启动图 MIME，仅支持 png / jpg。
fn splash_mime(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        _ => None,
    }
}

/// 创建打包器界面的 WebView。
///
/// WebView2 的配置目录一旦被旧版残留进程锁住或损坏，CreateCoreWebView2Environment
/// 会直接返回 E_UNEXPECTED(0x8000FFFF)，现象就是「创建 WebView 失败」。这里在首次
/// 失败时换一个全新目录再试一次，避免用户只能手动删缓存目录才能打开界面。
fn build_webview(
    window: &tao::window::Window,
    url: &str,
    ipc_proxy: &tao::event_loop::EventLoopProxy<UserEvent>,
) -> Result<wry::WebView> {
    let attempt = |dir_override: Option<String>| -> Result<wry::WebView> {
        if let Some(d) = dir_override {
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", d);
        }
        let proxy = ipc_proxy.clone();
        let mut builder = WebViewBuilder::new()
            .with_url(url)
            .with_ipc_handler(move |req: Request<String>| {
                let _ = proxy.send_event(UserEvent::Ipc(req.into_body()));
            });
        // 诊断用：EXEIFY_UI_SCROLL=<区块 id> 时，页面加载完成后自动滚动到该区块；
        // 写成 show:<元素 id> 则显示该隐藏元素（例如关于弹层），便于无人值守截图核验。
        if let Ok(sec) = std::env::var("EXEIFY_UI_SCROLL") {
            let js = match sec.strip_prefix("show:") {
                Some(id) => format!(
                    "var e=document.getElementById({}); if(e) e.classList.remove('hidden');",
                    js_str(id)
                ),
                None => format!(
                    "document.getElementById({}) && document.getElementById({}).scrollIntoView({{block:'start'}});",
                    js_str(&sec),
                    js_str(&sec)
                ),
            };
            let p = ipc_proxy.clone();
            builder = builder.with_on_page_load_handler(move |event, _u| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    let _ = p.send_event(UserEvent::Eval(js.clone()));
                }
            });
        }
        builder.build(window).context("创建 WebView 失败")
    };

    match attempt(None) {
        Ok(w) => Ok(w),
        Err(first) => {
            let alt = fresh_data_dir();
            match alt {
                Some(dir) => attempt(Some(dir)).map_err(|second| {
                    anyhow::anyhow!("{first:#}；备用配置目录重试也失败：{second:#}")
                }),
                None => Err(first),
            }
        }
    }
}

/// 在现有配置目录旁生成一个全新的目录（带时间戳，确保不会撞上被锁的旧目录）。
fn fresh_data_dir() -> Option<String> {
    let base = std::env::var_os("WEBVIEW2_USER_DATA_FOLDER")?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let dir = std::path::PathBuf::from(base).with_file_name(format!("alt-{stamp}"));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.to_string_lossy().into_owned())
}

fn js_str(s: &str) -> String {
    // 用 serde 生成安全的 JS 字符串字面量
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

pub fn run() -> Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Exeify — 网页打包器")
        .with_inner_size(LogicalSize::new(880.0, 640.0))
        .with_min_inner_size(LogicalSize::new(760.0, 560.0))
        .with_resizable(true)
        .with_window_icon(crate::app_window_icon())
        .build(&event_loop)
        .context("创建窗口失败")?;

    let ipc_proxy = proxy.clone();
    // 界面资源同样用本地回环 HTTP 服务器提供（与产物 runner 一致），
    // 因为自定义协议 `app://` 在部分 WebView2 版本上会在握手阶段直接失败。
    let port = crate::server::start(ui_map(), "index.html".to_string()).context("启动界面服务失败")?;
    let ui_url = format!("http://127.0.0.1:{port}/");
    let webview = build_webview(&window, &ui_url, &ipc_proxy)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Eval(js)) => {
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::Ipc(msg)) => {
                handle_ipc(&msg, &proxy, &webview);
            }
            _ => {}
        }
    });
}

fn handle_ipc(
    msg: &str,
    proxy: &tao::event_loop::EventLoopProxy<UserEvent>,
    webview: &wry::WebView,
) {
    let parsed: IpcMsg = match serde_json::from_str(msg) {
        Ok(m) => m,
        Err(e) => {
            let _ = webview.evaluate_script(&format!(
                "window.__log({});",
                js_str(&format!("消息解析失败：{e}"))
            ));
            return;
        }
    };

    match parsed.action.as_str() {
        // 选择本地目录 —— 在主线程弹原生对话框，并自动识别网页入口
        "pickFolder" => {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                let path = folder.to_string_lossy().to_string();
                let entries = packer::detect_html_entries(&folder);
                let payload = serde_json::json!({
                    "folder": path,
                    "entries": entries,
                    "entry": entries.first().cloned().unwrap_or_default(),
                });
                let _ = webview.evaluate_script(&format!("window.__onFolderPicked({});", payload));
            }
        }
        // 用系统默认浏览器打开外部链接（避免把打包器界面导航走）
        "openExternal" => {
            if let Some(url) = parsed.url.as_deref() {
                if url.starts_with("http://") || url.starts_with("https://") {
                    let _ = std::process::Command::new("explorer").arg(url).spawn();
                }
            }
        }
        // 选择图标文件（.ico/.png）
        "pickIcon" => {
            let dialog = rfd::FileDialog::new().add_filter("图标", &["ico", "png"]);
            if let Some(path) = dialog.pick_file() {
                let s = path.to_string_lossy().to_string();
                let preview = icon_data_url(&path).unwrap_or_default();
                let _ = webview.evaluate_script(&format!(
                    "window.__setIcon({}, {});",
                    js_str(&s),
                    js_str(&preview)
                ));
            }
        }
        // 选择启动图（.png/.jpg）
        "pickSplash" => {
            let dialog = rfd::FileDialog::new().add_filter("图片", &["png", "jpg", "jpeg"]);
            if let Some(path) = dialog.pick_file() {
                let s = path.to_string_lossy().to_string();
                let preview = splash_data_url(&path).unwrap_or_default();
                let _ = webview.evaluate_script(&format!(
                    "window.__setSplash({}, {});",
                    js_str(&s),
                    js_str(&preview)
                ));
            }
        }
        // 选择输出 exe 路径
        "pickOutput" => {
            let default_name = parsed.default_name.unwrap_or_else(|| "app.exe".to_string());
            let dialog = rfd::FileDialog::new()
                .set_file_name(&default_name)
                .add_filter("可执行文件", &["exe"]);
            if let Some(path) = dialog.save_file() {
                let mut path = path;
                if path.extension().is_none() {
                    path.set_extension("exe");
                }
                let s = path.to_string_lossy().to_string();
                let _ = webview.evaluate_script(&format!("window.__setOutput({});", js_str(&s)));
            }
        }
        // 选择输出 apk 路径
        "pickApk" => {
            let default_name = parsed.default_name.unwrap_or_else(|| "app.apk".to_string());
            let dialog = rfd::FileDialog::new()
                .set_file_name(&default_name)
                .add_filter("安卓安装包", &["apk"]);
            if let Some(path) = dialog.save_file() {
                let mut path = path;
                if path.extension().is_none() {
                    path.set_extension("apk");
                }
                let s = path.to_string_lossy().to_string();
                let _ = webview.evaluate_script(&format!("window.__setApk({});", js_str(&s)));
            }
        }
        // 打包 —— 放到后台线程，完成后回调
        "pack" => {
            let Some(req) = parsed.data else {
                let _ = webview.evaluate_script(&format!(
                    "window.__packResult(false, {});",
                    js_str("缺少打包参数")
                ));
                return;
            };
            let proxy = proxy.clone();
            std::thread::spawn(move || {
                let result = do_pack(req);
                let js = match result {
                    Ok(out) => format!(
                        "window.__packResult(true, {});",
                        js_str(&format!("打包成功：{out}"))
                    ),
                    Err(e) => format!("window.__packResult(false, {});", js_str(&format!("{e}"))),
                };
                let _ = proxy.send_event(UserEvent::Eval(js));
            });
        }
        other => {
            let _ = webview.evaluate_script(&format!(
                "window.__log({});",
                js_str(&format!("未知动作：{other}"))
            ));
        }
    }
}

fn do_pack(req: PackReq) -> Result<String> {
    let window = WindowCfg {
        title: if req.title.trim().is_empty() {
            "App".to_string()
        } else {
            req.title.clone()
        },
        width: if req.width > 0.0 { req.width } else { 1024.0 },
        height: if req.height > 0.0 { req.height } else { 720.0 },
        resizable: req.resizable,
        state: parse_state(req.state.as_deref()),
    };
    let output = PathBuf::from(&req.output);
    if req.output.trim().is_empty() {
        anyhow::bail!("请先选择输出路径");
    }

    let icon_path = req
        .icon
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let icon = icon_path.as_deref();

    let splash = build_splash(&req)?;

    let splash_bytes = req
        .splash
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .and_then(|p| std::fs::read(p).ok());

    let entry = req
        .entry
        .clone()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "index.html".to_string());

    let source_dir: Option<PathBuf>;
    let source_url: Option<String>;

    match req.mode.as_str() {
        "url" => {
            let url = req.url.clone().unwrap_or_default();
            packer::pack_url(&url, window.clone(), &output, icon, splash.clone())?;
            source_dir = None;
            source_url = Some(url);
        }
        "local" => {
            let folder = req.folder.clone().unwrap_or_default();
            if folder.trim().is_empty() {
                anyhow::bail!("请先选择本地网页目录");
            }
            packer::pack_local(
                &PathBuf::from(&folder),
                &entry,
                window.clone(),
                &output,
                icon,
                splash.clone(),
            )?;
            source_dir = Some(PathBuf::from(folder));
            source_url = None;
        }
        m => anyhow::bail!("未知模式：{m}"),
    }

    // 可选：同时生成安卓 APK（与 exe 同名，扩展名换成 .apk）
    let mut apk_note = String::new();
    if req.make_apk.unwrap_or(false) {
        let apk_path = req
            .apk
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| output.with_extension("apk"));
        crate::apk::build_apk(&crate::apk::ApkSpec {
            title: &window.title,
            entry: &entry,
            url: source_url.as_deref(),
            www_dir: source_dir.as_deref(),
            icon,
            splash: splash_bytes.as_deref(),
            splash_ms: if splash_bytes.is_some() { 1500 } else { 0 },
            fullscreen: matches!(window.state, WindowState::Fullscreen),
            output: &apk_path,
        })?;
        apk_note = format!("；APK：{}", apk_path.display());
    }

    Ok(format!("{}{}", output.display(), apk_note))
}

/// 把前端下拉值解析成窗口状态，未知/缺省时为普通窗口。
fn parse_state(s: Option<&str>) -> WindowState {
    match s {
        Some("maximized") => WindowState::Maximized,
        Some("fullscreen") => WindowState::Fullscreen,
        _ => WindowState::Normal,
    }
}

/// 由启动图路径读取字节并组装启动页配置。未选启动图返回 None。
fn build_splash(req: &PackReq) -> Result<Option<SplashCfg>> {
    let Some(path) = req
        .splash
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    let mime = splash_mime(&path)
        .ok_or_else(|| anyhow::anyhow!("启动图仅支持 png / jpg"))?
        .to_string();
    let bytes =
        std::fs::read(&path).with_context(|| format!("读取启动图失败：{}", path.display()))?;
    // 默认 1500ms；背景色默认深石板，与设计文档一致。
    let min_ms = req.splash_ms.unwrap_or(1500);
    let bg = req
        .splash_bg
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("#0f172a")
        .to_string();
    Ok(Some(SplashCfg {
        image_b64: crate::b64::encode(&bytes),
        mime,
        min_ms,
        bg,
        fit: "cover".to_string(),
    }))
}

// ---- 内嵌 UI 资源 ----

/// 界面资源表：键为相对路径，值为文件字节。由本地回环服务器提供。
fn ui_map() -> std::collections::HashMap<String, Vec<u8>> {
    let mut m = std::collections::HashMap::new();
    m.insert(
        "index.html".to_string(),
        include_bytes!("../ui/index.html").to_vec(),
    );
    m.insert(
        "style.css".to_string(),
        include_bytes!("../ui/style.css").to_vec(),
    );
    m.insert(
        "app.js".to_string(),
        include_bytes!("../ui/app.js").to_vec(),
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 界面（ui/app.js）发出的 pack 消息必须能被 Rust 端完整解析，
    /// 尤其是新增的 make_apk / apk 两个字段。
    #[test]
    fn pack_message_from_ui_parses() {
        // 与 ui/app.js 中 buildPayload() 生成的 JSON 保持同构
        let json = r##"{"action":"pack","data":{
            "mode":"local","url":"","folder":"C:\\demo",
            "entry":"index.html","icon":"","title":"GUI 测试",
            "width":1024,"height":720,"resizable":true,
            "output":"C:\\demo\\app.exe","state":"normal",
            "splash":"","splash_ms":1500,"splash_bg":"#0f172a",
            "make_apk":true,"apk":"C:\\demo\\app.apk"
        }}"##;
        let msg: IpcMsg = serde_json::from_str(json).expect("界面消息应可解析");
        assert_eq!(msg.action, "pack");
        let req = msg.data.expect("pack 消息必须带 data");
        assert_eq!(req.mode, "local");
        assert_eq!(req.make_apk, Some(true));
        assert_eq!(req.apk.as_deref(), Some("C:\\demo\\app.apk"));
        assert_eq!(req.splash_ms, Some(1500));
    }

    /// 兼容旧版界面：不传 make_apk / apk 时不应报错，默认不生成 apk。
    #[test]
    fn pack_message_without_apk_fields_parses() {
        let json = r#"{"action":"pack","data":{
            "mode":"url","url":"https://example.com","folder":"",
            "entry":"","icon":"","title":"旧界面",
            "width":1024,"height":720,"resizable":true,
            "output":"C:\\demo\\app.exe"
        }}"#;
        let msg: IpcMsg = serde_json::from_str(json).expect("旧版消息应向后兼容");
        let req = msg.data.expect("pack 消息必须带 data");
        assert_eq!(req.make_apk, None);
        assert_eq!(req.apk, None);
    }
}

