# 失败症状与排查

按「看到什么 → 先查什么 → 怎么修」组织。只列实际遇到过或可从代码路径推断的情形，不要凭猜测改参数。

## 打包器起不来 / 界面白屏 / 报 HRESULT(0x8000FFFF)

- 症状：双击 `exeify.exe` 无界面，或命令行报 `WebView2 error: ... HRESULT(0x8000FFFF) 灾难性故障`。
- 根因：WebView2 用户数据目录损坏或被残留进程锁住，`CreateCoreWebView2Environment` 返回 E_UNEXPECTED。
- 处置顺序：
  1. 关掉所有已运行的 `exeify.exe` 进程（旧进程持锁时新实例必然失败）。
  2. 删除 `%LOCALAPPDATA%\web2app\` 下对应 exe 名的目录（本工具的数据目录，与官方 exeify 的 `%LOCALAPPDATA%\exeify\` 已隔离，删它不影响别的软件）。
  3. 再启动。工具自身在首次失败时也会自动换一个 `alt-<时间戳>` 新目录重试一次，错误信息里会同时带两次失败原因。
- 注意：命令行打包路径（`exeify pack ...`）不创建 GUI，不会触发这个故障；它只在打开图形界面时出现。

## `失败:` 里提到源目录 / 入口

- `--local` 指到不存在的目录、或该目录里没有 `index.html` 且没给 `--entry`：先 `Get-ChildItem` 确认目录与入口文件真实存在，再传绝对路径。
- 前端框架产物（Vite/React）的入口常在 `dist\index.html`，指向仓库根会失败；把 `--local` 指到构建输出目录。

## `失败:` 里提到图片 / 图标 / 启动图

- `--icon` / `--splash` 指向的图片无法解码：确认扩展名与实际格式一致（把 `.jpg` 改名成 `.png` 会失败）。
- 图标建议给方形 PNG（如 512×512 或 256×256）；索引色/带透明通道的 PNG 也能转，但极小尺寸（<32px）放大会糊。

## 输出写不进去

- `--out` / `--apk` 的目标目录不存在时先建目录；目标 exe/apk 正被运行或预览占用时先关闭再重试。
- 目标路径在受保护目录（如 `C:\Program Files`）且当前无管理员权限：换到用户目录。

## 产物 exe 能生成但双击没反应

- `--url` 模式的产物需要联网，断网会白屏；`--local` 模式是自包含的，断网仍可显示。
- 目标机器缺 WebView2（少见的精简版 Win10/Server）：装一次 WebView2 Runtime 即可，产物本身不用改。
- 首次启动会解包内嵌资源到临时目录，慢机器上首开可能等 1–2 秒，属正常。

## APK 校验失败

用 `python scripts/verify_apk.py <apk> [源目录]` 看它打印到哪一步失败：

- 「zip 条目损坏」：产物在传输/写盘时被截断，重新打包一次。
- 「v2 摘要不匹配 / 验签失败」：说明 apk 被外部工具改写过（例如又用别的工具往里塞文件），重新打包，别再二次改包。
- 「assets/www 与源目录不一致」：`--entry` 指错文件，或源目录在打包过程中被改动；固定源目录后重打。
- 只要签名段能验通，这个 apk 就能被 Android 7+ 安装（需允许未知来源）。

## 用户问「能不能不暴露源码 / 加密保护」

- 本版本把网页资源自包含附在 exe 尾部，属于**打包而非加密**，有工具可以抽出来。
- 需要更强保护时如实说明做不到「不可提取」，可建议后端化（把敏感逻辑放服务端），不要承诺加密。

## 用户要「换成自己的签名证书 / 上架要用」

先说清事实，别含糊：

- 本版命令行的 APK 签名**固定使用内置 demo 密钥**（`CN=Exeify Demo`，私钥随仓库公开），**没有** `--keystore` 之类参数。任何拿到该私钥的人都能签出同包名的包，所以**绝对不要拿它对外分发或上架**。
- 用户只是自己侧载 / 内部小范围用：可以继续用，但要在交付里写明签名是 demo 的。
- 用户要正式分发，给两条可执行路径：

**A. 改源码换密钥后重新编译**（改动最小、最干净的路线）

```bash
# 1) 生成自有密钥对（RSA 2048，有效期可自定）
keytool -genkeypair -v -keystore my-release.jks -alias mykey -keyalg RSA -keysize 2048 -validity 36500 -storepass <密码> -keypass <密码> -dname "CN=公司名, O=组织, C=CN"
# 2) 导出私钥（PKCS#8 / DER）与证书（DER）
keytool -importkeystore -srckeystore my-release.jks -destkeystore my.p12 -deststoretype PKCS12 -srcalias mykey
openssl pkcs12 -in my.p12 -nocerts -nodes -out my.key.pem
openssl pkcs8 -topk8 -inform PEM -outform DER -in my.key.pem -out private.der -nocrypt
keytool -exportcert -rfc -keystore my-release.jks -alias mykey -storepass <密码> -file my.crt.pem
openssl x509 -in my.crt.pem -outform DER -out cert.der
```
把 `private.der`（私钥）与 `cert.der`（证书）替换 `assets/apk/key.pem` 中的对应内容，然后 `cargo build --release`，之后打的 apk 就是自有签名。

**B. 对已生成的 apk 直接重签**（产物不用重新打包）

```bash
apksigner sign --ks my-release.jks --ks-key-alias mykey --out app-signed.apk app.apk
apksigner verify -v app-signed.apk
```
必须提醒：重签后本 skill 的 `scripts/verify_apk.py` 会报「验签不匹配」，因为它按内置证书校验 —— 这是**预期行为**，不是包坏了；改用 `apksigner verify` 校验即可。

## 目录里的文件删不掉（Access denied / 删完仍在）

- 症状：某项目目录下所有文件都删不掉，连自己刚新建的探针文件也删不掉；文件本身无锁、可读可复制。
- 说明：这是宿主对该目录的文件系统保护，与打包器无关。**不要**反复换删除方式（`Remove-Item` / `[IO.File]::Delete` / 改名 / 移动到别处都无效），也不要为此改成"覆盖内容"来假装清理干净。
- 正确处置：确认该文件是否被代码引用、是否已被 `.gitignore` 排除、是否曾进入 git 历史；三项都为「否」就算处理到位，如实告知用户受环境限制需自行删除。若确实要物理删除，把项目整体搬到不受保护的目录（如 `D:\work\`）后再删。

