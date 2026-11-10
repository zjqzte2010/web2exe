// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 不坑老师 · https://github.com/44886/Exeify
// Copyright 2026 zjqzte2010 · https://github.com/zjqzte2010/web2exe（二次开发）
//
// 发布版隐藏控制台窗口；debug 版保留控制台便于观察日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apk;
mod b64;
mod config;
mod gui;
mod icon;
mod packer;
mod payload;
mod runner;
mod server;

/// 自定义协议 `app` 的起始地址（保留给旧产物兼容；界面与产物现已统一走本地回环 HTTP）。
#[allow(dead_code)]
pub fn asset_base_url() -> &'static str {
    #[cfg(windows)]
    {
        "http://app.localhost/"
    }
    #[cfg(not(windows))]
    {
        "app://localhost/"
    }
}

/// 应用窗口图标（来自内嵌的 `assets/icon.png`），用于任务栏与标题栏。
/// tao/winit 默认不会用 exe 的 PE 图标，必须运行时显式设置。
pub fn app_window_icon() -> Option<tao::window::Icon> {
    const PNG: &[u8] = include_bytes!("../assets/icon.png");
    let (w, h, rgba) = icon::decode_png_rgba_bytes(PNG).ok()?;
    tao::window::Icon::from_rgba(rgba, w, h).ok()
}

fn main() {
    // 把 WebView2 的用户数据目录重定向到 %LOCALAPPDATA%，
    // 避免在 exe 旁边生成 <名字>.exe.WebView2 缓存目录。
    redirect_webview_data_dir();

    // 隐藏 CLI：便于脚本化打包与自测（GUI 用户不会用到）。
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2
        && matches!(
            args[1].as_str(),
            "pack" | "pack-local" | "pack-url" | "version" | "--version" | "-V"
        )
    {
        std::process::exit(run_cli(&args));
    }

    match payload::read_from_current_exe() {
        // 尾部带载荷 -> runner 模式
        Ok(Some(p)) => {
            if let Err(e) = runner::run(p) {
                fatal(&format!("启动失败：{e:#}"));
            }
        }
        // 无载荷 -> GUI 打包器模式
        Ok(None) => {
            if let Err(e) = gui::run() {
                fatal(&format!("界面启动失败：{e:#}"));
            }
        }
        Err(e) => {
            // 读取载荷出错也退回 GUI，避免完全打不开
            eprintln!("读取载荷出错：{e:#}，退回打包器界面");
            if let Err(e) = gui::run() {
                fatal(&format!("界面启动失败：{e:#}"));
            }
        }
    }
}

/// 将 WebView2 用户数据目录设置到 `%LOCALAPPDATA%\web2app\<exe名>`。
/// 用独立的 `web2app` 根目录，避免与官方 Exeify 的 `%LOCALAPPDATA%\exeify` 配置
/// 目录互相踩踏（同名目录被旧版残留锁/损坏时，WebView2 创建会直接返回 E_UNEXPECTED）。
/// 若用户已通过环境变量指定，则尊重其设置。
fn redirect_webview_data_dir() {
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_some() {
        return;
    }
    let Some(base) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let stem = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "app".to_string());
    let safe: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "app".to_string()
    } else {
        safe
    };
    let dir = std::path::PathBuf::from(base).join("web2app").join(safe);
    if std::fs::create_dir_all(&dir).is_ok() {
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &dir);
    }
}

/// 隐藏 CLI 入口。两种写法都支持：
///
/// 命名参数（推荐）：
///   exeify pack --local <目录> [--entry index.html] --out <输出.exe> [--apk <输出.apk>] [选项]
///   exeify pack --url <网址> --out <输出.exe> [--apk <输出.apk>] [选项]
///   `--out` 与 `--apk` 至少给一个：只给 --out 就只出 exe，只给 --apk 就只出 apk，
///   两个都给就一次出双端。
///
/// 旧式位置参数（兼容早期调用）：
///   exeify pack-local <目录> <输出.exe> [入口] [图标] [--apk <输出.apk>]
///   exeify pack-url   <网址> <输出.exe> [图标] [--apk <输出.apk>]
///
/// 通用选项：`--title 名称`、`--width 宽`、`--height 高`、
/// `--window normal|maximized|fullscreen`、`--no-resizable`、
/// `--icon <.ico|.png>`、`--splash <.png|.jpg>`、`--splash-bg #rrggbb`、`--splash-sec 秒`
fn run_cli(args: &[String]) -> i32 {
    match args[1].as_str() {
        "version" | "--version" | "-V" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        "pack" => pack_named(&args[2..]),
        "pack-local" | "pack-url" => pack_legacy(args),
        other => {
            eprintln!("未知命令：{other}");
            print_usage();
            2
        }
    }
}

/// 一次打包的全部选项。
#[derive(Debug, Default)]
struct Opts {
    local: Option<std::path::PathBuf>,
    url: Option<String>,
    entry: String,
    out: Option<std::path::PathBuf>,
    apk: Option<std::path::PathBuf>,
    icon: Option<std::path::PathBuf>,
    splash: Option<std::path::PathBuf>,
    splash_bg: String,
    splash_ms: u64,
    title: Option<String>,
    width: Option<f64>,
    height: Option<f64>,
    resizable: bool,
    window: Option<String>,
}

fn print_usage() {
    eprintln!(
        "用法:\n  exeify pack --local <目录> [--entry index.html] --out <输出.exe> [--apk <输出.apk>] [选项]\n  exeify pack --url <网址> --out <输出.exe> [--apk <输出.apk>] [选项]\n选项: --title 名称 --width 宽 --height 高 --window normal|maximized|fullscreen --no-resizable --icon <.ico|.png> --splash <.png|.jpg> --splash-bg #rrggbb --splash-sec 秒\n说明: --out 与 --apk 至少给一个；两个都给即一次产出 exe + apk\n      exeify version  打印版本号"
    );
}

/// 解析 `pack` 子命令的命名参数。
fn pack_named(rest: &[String]) -> i32 {
    let mut o = Opts {
        entry: "index.html".to_string(),
        splash_bg: "#0f172a".to_string(),
        splash_ms: 1500,
        resizable: true,
        ..Default::default()
    };
    let mut i = 0;
    // 取下一个值，缺值直接报用法错误
    macro_rules! val {
        ($flag:expr) => {{
            if i + 1 >= rest.len() {
                eprintln!("{} 缺少取值", $flag);
                print_usage();
                return 2;
            }
            i += 1;
            rest[i].clone()
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--local" => o.local = Some(val!("--local").into()),
            "--url" => o.url = Some(val!("--url")),
            "--entry" => o.entry = val!("--entry"),
            "--out" => o.out = Some(val!("--out").into()),
            "--apk" => o.apk = Some(val!("--apk").into()),
            "--icon" => o.icon = Some(val!("--icon").into()),
            "--splash" => o.splash = Some(val!("--splash").into()),
            "--splash-bg" => o.splash_bg = val!("--splash-bg"),
            "--splash-sec" => {
                let v = val!("--splash-sec");
                match v.trim().parse::<f64>() {
                    Ok(s) if s >= 0.0 => o.splash_ms = (s * 1000.0).round() as u64,
                    _ => {
                        eprintln!("--splash-sec 需要非负数字");
                        return 2;
                    }
                }
            }
            "--title" => o.title = Some(val!("--title")),
            "--width" => match val!("--width").trim().parse::<f64>() {
                Ok(w) if w > 0.0 => o.width = Some(w),
                _ => {
                    eprintln!("--width 需要正数");
                    return 2;
                }
            },
            "--height" => match val!("--height").trim().parse::<f64>() {
                Ok(h) if h > 0.0 => o.height = Some(h),
                _ => {
                    eprintln!("--height 需要正数");
                    return 2;
                }
            },
            "--window" => o.window = Some(val!("--window")),
            "--no-resizable" => o.resizable = false,
            "--help" | "-h" => {
                print_usage();
                return 0;
            }
            other => {
                eprintln!("未知参数：{other}");
                print_usage();
                return 2;
            }
        }
        i += 1;
    }
    finish(o)
}

/// 兼容旧式位置参数，转换为同一套 Opts。
fn pack_legacy(args: &[String]) -> i32 {
    let mut apk_out: Option<std::path::PathBuf> = None;
    let mut apk_only = false;
    let mut title: Option<String> = None;
    let mut window: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--apk-only" => {
                apk_only = true;
                i += 1;
            }
            "--apk" => {
                if i + 1 >= args.len() {
                    eprintln!("--apk 缺少路径");
                    return 2;
                }
                apk_out = Some(std::path::PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--title" => {
                if i + 1 >= args.len() {
                    eprintln!("--title 缺少文字");
                    return 2;
                }
                title = Some(args[i + 1].clone());
                i += 2;
            }
            "--window" => {
                if i + 1 >= args.len() {
                    eprintln!("--window 缺少模式");
                    return 2;
                }
                window = Some(args[i + 1].clone());
                i += 2;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }

    let min = if apk_only { 1 } else { 2 };
    if positional.len() < min {
        print_usage();
        return 2;
    }
    let mut o = Opts {
        entry: "index.html".to_string(),
        splash_bg: "#0f172a".to_string(),
        splash_ms: 1500,
        resizable: true,
        title,
        window,
        apk: apk_out,
        ..Default::default()
    };
    if args[1] == "pack-local" {
        o.local = Some(positional[0].clone().into());
        if !apk_only {
            o.out = Some(positional[1].clone().into());
            if let Some(e) = positional.get(2) {
                o.entry = e.clone();
            }
            if let Some(ic) = positional.get(3) {
                o.icon = Some(ic.clone().into());
            }
        } else if let Some(ic) = positional.get(1) {
            o.icon = Some(ic.clone().into());
        }
    } else {
        o.url = Some(positional[0].clone());
        if !apk_only {
            o.out = Some(positional[1].clone().into());
            if let Some(ic) = positional.get(2) {
                o.icon = Some(ic.clone().into());
            }
        } else if let Some(ic) = positional.get(1) {
            o.icon = Some(ic.clone().into());
        }
    }
    finish(o)
}

/// 校验选项并执行打包，打印 `OK: <路径>`。
fn finish(o: Opts) -> i32 {
    use config::{SplashCfg, WindowCfg, WindowState};

    if o.local.is_none() && o.url.is_none() {
        eprintln!("失败: 必须指定 --local <目录> 或 --url <网址>");
        return 2;
    }
    if o.local.is_some() && o.url.is_some() {
        eprintln!("失败: --local 与 --url 只能二选一");
        return 2;
    }
    if o.out.is_none() && o.apk.is_none() {
        eprintln!("失败: --out 与 --apk 至少给一个（不给 --apk 就不生成 APK）");
        return 2;
    }

    let mut win = WindowCfg::default();
    if let Some(t) = o.title.as_ref().filter(|t| !t.trim().is_empty()) {
        win.title = t.clone();
    }
    if let Some(w) = o.width {
        win.width = w;
    }
    if let Some(h) = o.height {
        win.height = h;
    }
    win.resizable = o.resizable;
    if let Some(m) = o.window.as_deref() {
        win.state = match m {
            "normal" => WindowState::Normal,
            "maximized" => WindowState::Maximized,
            "fullscreen" => WindowState::Fullscreen,
            other => {
                eprintln!("未知窗口模式：{other}（可选 normal / maximized / fullscreen）");
                return 2;
            }
        };
    }

    // 启动页（可选）
    let splash: Option<SplashCfg> = match o.splash.as_deref() {
        None => None,
        Some(p) => match build_splash_cfg(p, o.splash_ms, &o.splash_bg) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("失败: {e:#}");
                return 1;
            }
        },
    };
    let splash_bytes: Option<Vec<u8>> = match o.splash.as_deref() {
        None => None,
        Some(p) => std::fs::read(p).ok(),
    };

    let icon = o.icon.as_deref();
    let result: anyhow::Result<Vec<std::path::PathBuf>> = (|| {
        let mut produced = Vec::new();
        if let Some(out) = o.out.as_deref() {
            match (o.local.as_deref(), o.url.as_deref()) {
                (Some(dir), _) => {
                    packer::pack_local(dir, &o.entry, win.clone(), out, icon, splash.clone())?
                }
                (None, Some(url)) => packer::pack_url(url, win.clone(), out, icon, splash.clone())?,
                (None, None) => unreachable!("已在 finish 开头校验过来源"),
            }
            produced.push(out.to_path_buf());
        }
        if let Some(apk_path) = o.apk.as_deref() {
            apk::build_apk(&apk::ApkSpec {
                title: &win.title,
                entry: &o.entry,
                url: o.url.as_deref(),
                www_dir: o.local.as_deref(),
                icon,
                splash: splash_bytes.as_deref(),
                splash_ms: if splash_bytes.is_some() {
                    o.splash_ms as u32
                } else {
                    0
                },
                fullscreen: matches!(win.state, WindowState::Fullscreen),
                output: apk_path,
            })?;
            produced.push(apk_path.to_path_buf());
        }
        Ok(produced)
    })();

    match result {
        Ok(list) => {
            for p in list {
                println!("OK: {}", p.display());
            }
            0
        }
        Err(e) => {
            eprintln!("失败: {e:#}");
            1
        }
    }
}

/// 读取启动图并组装启动页配置（仅 png / jpg）。
fn build_splash_cfg(
    path: &std::path::Path,
    min_ms: u64,
    bg: &str,
) -> anyhow::Result<config::SplashCfg> {
    use anyhow::{anyhow, Context};
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => return Err(anyhow!("启动图仅支持 png / jpg：{}", path.display())),
    };
    let bytes =
        std::fs::read(path).with_context(|| format!("读取启动图失败：{}", path.display()))?;
    Ok(config::SplashCfg {
        image_b64: b64::encode(&bytes),
        mime: mime.to_string(),
        min_ms,
        bg: if bg.trim().is_empty() {
            "#0f172a".to_string()
        } else {
            bg.trim().to_string()
        },
        fit: "cover".to_string(),
    })
}

fn fatal(msg: &str) {
    eprintln!("{msg}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Exeify")
        .set_description(msg)
        .show();
}
