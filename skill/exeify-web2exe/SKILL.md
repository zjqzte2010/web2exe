---
name: exeify-web2exe
description: "把网页打包成 Windows exe（可同时产出安卓 APK）的技能。当用户想把「一个本地 HTML/CSS/JS 网页文件夹」或「一个在线网址」做成双击即运行、免安装的独立 Windows .exe（用系统自带 WebView2 渲染，终端用户无需装任何运行时）时使用；支持窗口标题/宽高/启动模式 normal|maximized|fullscreen、自定程序图标（.png/.ico）、启动图、禁止缩放；一次调用可只出 exe、只出 apk，或同一次产出 exe + apk（APK 免 Android SDK 本地生成）。仅 Windows 上运行打包器。 | Use when the user wants to turn a website — a local HTML/CSS/JS folder or an online URL — into a standalone, double-click-to-run Windows .exe rendered by the system WebView2, optionally outputting an Android APK from the same site in one run, with window title/size/start-mode, custom icon, splash screen options. The packer runs on Windows only. | 触发词：打包成exe、网页打包、网页转exe、把网址做成桌面程序、把文件夹做成exe、网站变桌面应用、网页转apk、打包成安卓app、把网页做成安卓应用、web2exe、exeify、package website to exe、HTML folder to desktop app、website to exe、website to apk、web to android app。"
---

# Exeify 网页打包器（网页 → Windows exe，可选 → 安卓 APK）

用本 skill 内置的 `exeify.exe` 把网页打成 **Windows 独立 exe**，需要时同一次调用再产出 **安卓 APK**。
打包器与本 `SKILL.md` 在**同一目录**，产物体积约 1.4 MB（exe）/ 45 KB 起（apk），不依赖 Node / Rust / Android SDK。

## 1. 前置检查

1. **仅 Windows**。当前环境不是 Windows 就直接告知用户此技能只能在 Windows 上跑，停止，不要尝试在 Linux/macOS 上执行该 exe。
2. **定位 exeify.exe**：与本文件同目录下的 `exeify.exe`（即 `<SKILL.md 所在目录>\exeify.exe`）。
   - 该文件不在时，先看仓库 `skill/exeify-web2exe/` 或从 release 取回：
     `https://github.com/zjqzte2010/web2exe/releases/download/v0.2.1/exeify.exe`
   - 下载后放到本 skill 目录再继续；用户网络访问不通就直说打包器缺失、无法打包。
3. 跑一次 `exeify.exe version`（秒退），确认可执行、记下版本号（本 skill 随附 0.2.1）。
   单独一行执行、用退出码判断即可；不要把它的输出拼进 shell 表达式里捕获（个别 shell 会提前关闭管道，进程会报 panic，与打包能力无关）。

## 2. 先问清这四项（缺就问，不要猜）

| 要问 | 说明 |
|---|---|
| 源 | **本地网页目录**（含 `index.html` 的文件夹）**或**在线网址（`http`/`https`），二选一 |
| 输出 | 产物路径与文件名（`.exe` / `.apk`），可放在源目录旁 |
| 要不要 APK | 只出 exe、只出 apk，还是两个都出（`--out` 与 `--apk` 至少给一个） |
| 可选项 | 窗口标题、宽高、启动模式、程序图标 `.ico/.png`、启动图、是否禁止缩放窗口 |

## 3. 调用命令

```
exeify.exe pack --local <目录> [--entry index.html] --out <输出.exe> [--apk <输出.apk>] [选项]
exeify.exe pack --url <网址> --out <输出.exe> [--apk <输出.apk>] [选项]
```

| 参数 | 说明 | 默认 |
|---|---|---|
| `--local <目录>` / `--url <网址>` | 源，二选一必填 | — |
| `--out <路径.exe>` | 输出 Windows exe | 无 |
| `--apk <路径.apk>` | 输出安卓 APK（应用名取 `--title`，图标取 `--icon`，启动图取 `--splash`，`--window fullscreen` 时 APK 全屏；包名自动生成为 `com.exeify.<hex>`） | 无 |
| `--entry <文件>` | 本地入口文件（仅 `--local`） | `index.html` |
| `--title <文字>` | 窗口标题 / APK 应用名 | `App` |
| `--width <数>` `--height <数>` | 窗口宽 / 高 | 1024 / 720 |
| `--window normal\|maximized\|fullscreen` | 启动模式 | `normal` |
| `--no-resizable` | 禁止缩放窗口 | 允许 |
| `--icon <.ico\|.png>` | 程序图标（PNG 会自动转多尺寸 ICO，产物 exe 与 APK 共用） | 内置默认图标 |
| `--splash <.png\|.jpg>` | 启动图，消除首屏白屏 | 无 |
| `--splash-bg #rrggbb` | 启动图背景色 | `#0f172a` |
| `--splash-sec <秒>` | 启动图最少显示时长 | 1.5 |

- `--out` 与 `--apk` **至少一个**；两个都给就是一次产出 exe + apk。
- 本版本**没有** `--no-protect` 之类的开关；不要发明参数。拼错参数会打印用法并以返回码 2 退出。
- 完整用法随时打印：`exeify.exe pack --help`。
- 路径含空格必须加引号（PowerShell 用 `& "路径\exeify.exe"` 调用）。

## 4. 判定成功 / 失败

| 返回码 | 输出 | 含义 |
|---|---|---|
| 0 | 一行 `OK: <产物路径>` | 成功 |
| 1 | `失败: <原因>` | 打包错误（源目录不存在、入口文件找不到、图片无法解码、写盘失败等） |
| 2 | 打印用法 | 参数错误 |

**不要只看返回码**：成功后必须确认输出文件真的存在、大小合理（exe 约 1.3–1.5 MB，apk 约 45 KB 起），再把路径告诉用户。失败就把 `失败:` 后面的原因转述给用户并修正参数重试，不要重复原样命令。

## 5. 产物验收（建议每次都做）

- **exe**：让用户双击试，或用 `Start-Process` 启动后确认窗口出现（标题应为 `--title` 的值）、进程存活 2 秒以上。离线目录模式的产物断网也能显示页面；`--url` 模式的产物需要联网。
- **apk**：用本 skill 的校验脚本（不需要 Android SDK / apksigner）：
  ```
  python scripts/verify_apk.py <apk路径> [源网页目录]
  ```
  打印 `VERIFY PASSED` 表示：zip 条目完好、v2 签名三段摘要与 RSA 验签通过、`assets/www` 内页面与源目录一致、清单包名与中文标签正确。给了源目录才会做内容比对，只给 apk 也能查结构与签名。

## 6. 常见故障与边界

- **打包器自身界面打不开 / `HRESULT(0x8000FFFF)` 灾难性故障**：WebView2 用户数据目录被污染或残留进程占用。本工具已把配置目录隔离到 `%LOCALAPPDATA%\web2app\<exe名>` 并在首次失败时自动换一个全新目录重试；仍失败就清掉该目录后再开。
- **APK 是 demo 签名**：内置证书 `CN=Exeify Demo`，私钥公开在仓库里，只适合侧载/内部试用。用户要对外分发或上架应用市场时，必须明确提示换自己的 keystore 重签 —— 这个包不能直接上架。现版本没有 `--keystore` 参数；换密钥的两条可执行路径（改源码重编译 / `apksigner` 重签）见 `references/troubleshooting.md`。
- **安卓端要求**：Android 7+ 自带 System WebView；安装需允许「未知来源」。
- **体积**：exe 约 1.4 MB（运行时靠系统 WebView2，不内嵌引擎），apk 约 45 KB 起。用户提到体积上限（如 200 MB）时直接给实测字节数，不要估算。
- **资源是自包含附在 exe 尾部，不是加密保护**：不要向用户承诺「源码不可提取」。
- **非 Windows**：直接拒绝执行并说明原因，不要用 wine / 交叉编译替代。
- **不要为了联网检查新版本而阻塞打包**：离线可直接出包；版本检查是可选动作，失败就静默跳过。

## 7. 参考

- 更细的失败症状与排查路径：`references/troubleshooting.md`
- 仓库：https://github.com/zjqzte2010/web2exe （源码、Release、二次开发说明）
- 本项目基于「不坑老师」的 Exeify（Apache-2.0）二次开发，署名见本目录 `NOTICE`；再分发时保留 `LICENSE` 与 `NOTICE`。
