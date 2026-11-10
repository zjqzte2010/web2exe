// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 不坑老师 · https://github.com/44886/Exeify
// Copyright 2026 zjqzte2010 · https://github.com/zjqzte2010/web2exe（二次开发）
//
// 本文件为派生改动：新增 Android APK 输出能力（复用内置 WebView 壳），
// 并在所有 zip 条目上启用 Zip64（large_file），修复打包大文件时
// `Large file option has not been set` 的失败。

//! APK 打包：把网页资源写入内置的 Android WebView 壳，按 APK Signature Scheme v2 签名。
//!
//! 设计要点：
//! * 壳（AndroidManifest.xml / resources.arsc / classes.dex）来自 assets/apk，编译期编入；
//! * 所有条目使用 Stored 存储 + Zip64（`.large_file(true)`），因此超过 4GB 不再报错；
//! * 签名只做 v2（与上游 0.7.x 一致），Android 7+ 可直接侧载安装。

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MANIFEST_TPL: &[u8] = include_bytes!("../assets/apk/AndroidManifest.xml");
const ARSC_TPL: &[u8] = include_bytes!("../assets/apk/resources.arsc");
const DEX_TPL: &[u8] = include_bytes!("../assets/apk/classes.dex");
const NOTICE_TPL: &[u8] = include_bytes!("../assets/apk/NOTICE.txt");
const DEFAULT_ICON: &[u8] = include_bytes!("../assets/apk/default_icon.png");
const CERT_PEM: &[u8] = include_bytes!("../assets/apk/cert.pem");
const KEY_PEM: &[u8] = include_bytes!("../assets/apk/key.pem");

/// APK 打包参数。
pub struct ApkSpec<'a> {
    /// 应用名（写入 manifest 的 android:label）
    pub title: &'a str,
    /// 网页入口（assets/www 内的相对路径）
    pub entry: &'a str,
    /// URL 模式的目标网址；本地模式为 None
    pub url: Option<&'a str>,
    /// 本地模式：网页目录；URL 模式为 None
    pub www_dir: Option<&'a Path>,
    /// 应用图标（可选，png/ico）
    pub icon: Option<&'a Path>,
    /// 启动图 PNG 字节（可选）
    pub splash: Option<&'a [u8]>,
    /// 启动图最短展示毫秒
    pub splash_ms: u32,
    /// 是否全屏
    pub fullscreen: bool,
    /// 输出 APK 路径
    pub output: &'a Path,
}

/// 生成 APK。返回实际写入的路径。
pub fn build_apk(spec: &ApkSpec) -> Result<PathBuf> {
    if spec.output.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("apk"))
        != Some(true)
    {
        bail!("APK 输出文件名必须以 .apk 结尾");
    }
    if let Some(parent) = spec.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let package = gen_package();
    let manifest = patch_manifest(MANIFEST_TPL, spec.title, &package)?;
    let config = build_config(spec);
    let icon_png = build_launcher_icon(spec.icon)?;

    // 中间文件放在系统临时目录，避免污染输出目录（也可能因句柄/清理延迟残留大文件）
    let tmp = std::env::temp_dir().join(format!("exeify-apk-{}.part", std::process::id()));
    {
        let file = File::create(&tmp)
            .with_context(|| format!("创建临时文件失败：{}", tmp.display()))?;
        let mut zip = zip::ZipWriter::new(BufWriter::new(file));
        // 关键：large_file(true) 开启 Zip64，超过 4GB 的条目/偏移不再报
        // "Large file option has not been set"。
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);

        write_bytes_entry(&mut zip, "AndroidManifest.xml", &manifest, opts)?;
        write_bytes_entry(&mut zip, "resources.arsc", ARSC_TPL, opts)?;
        write_bytes_entry(
            &mut zip,
            "res/mipmap-xxxhdpi-v4/ic_launcher.png",
            &icon_png,
            opts,
        )?;
        write_bytes_entry(&mut zip, "classes.dex", DEX_TPL, opts)?;
        write_bytes_entry(&mut zip, "assets/config.json", config.as_bytes(), opts)?;
        write_bytes_entry(&mut zip, "assets/NOTICE.txt", NOTICE_TPL, opts)?;
        if let Some(splash) = spec.splash {
            write_bytes_entry(&mut zip, "assets/splash.png", splash, opts)?;
        }
        if let Some(dir) = spec.www_dir {
            write_dir_entries(&mut zip, dir, "assets/www", opts)?;
        }
        let mut w = zip.finish().context("完成 APK zip 失败")?;
        w.flush().ok();
    }

    // 读取临时 zip 的中央目录位置，插入 v2 签名块并改写 EOCD。
    sign_in_place(&tmp, spec.output)?;
    // 清理中间文件（Windows 上句柄释放可能稍有延迟，失败重试一次并告警）
    if let Err(e) = std::fs::remove_file(&tmp) {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Err(e2) = std::fs::remove_file(&tmp) {
            eprintln!("警告：临时文件未能删除：{}（{}；{}）", tmp.display(), e, e2);
        }
    }
    Ok(spec.output.to_path_buf())
}

fn write_bytes_entry<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    name: &str,
    data: &[u8],
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    zip.start_file(name, opts)
        .with_context(|| format!("创建 zip 条目失败：{name}"))?;
    zip.write_all(data)
        .with_context(|| format!("写入 zip 条目失败：{name}"))?;
    Ok(())
}

/// 递归写入目录内容；大文件采用流式复制，避免整文件读入内存。
fn write_dir_entries<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    dir: &Path,
    prefix: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in std::fs::read_dir(&p)
            .with_context(|| format!("读取目录失败：{}", p.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .with_context(|| format!("计算相对路径失败：{}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            let name = format!("{prefix}/{rel}");
            zip.start_file(&name, opts)
                .with_context(|| format!("创建 zip 条目失败：{name}"))?;
            let mut f = File::open(&path)
                .with_context(|| format!("打开文件失败：{}", path.display()))?;
            std::io::copy(&mut f, zip)
                .with_context(|| format!("写入 zip 条目失败：{name}"))?;
        }
    }
    Ok(())
}

/// 自动生成包名（与上游 0.7.x 风格一致：com.exeify.<10 位十六进制>）。
fn gen_package() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(nanos.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    let digest = h.finalize();
    format!("com.exeify.{:010x}", &digest[..5].iter().fold(0u64, |a, b| (a << 8) | *b as u64) & 0xffffffffff)
}

fn build_config(spec: &ApkSpec) -> String {
    let mode = if spec.url.is_some() { "url" } else { "local" };
    let url = spec.url.unwrap_or("");
    format!(
        "{{\"mode\":\"{mode}\",\"url\":\"{}\",\"entry\":\"{}\",\"fullscreen\":{},\"splashMs\":{}}}",
        json_escape(url),
        json_escape(spec.entry),
        spec.fullscreen,
        spec.splash_ms.max(0)
    )
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 生成 192×192 的 launcher 图标 PNG；未提供图标时用内置默认图标。
fn build_launcher_icon(icon: Option<&Path>) -> Result<Vec<u8>> {
    let Some(icon) = icon else {
        return Ok(DEFAULT_ICON.to_vec());
    };
    let (w, h, rgba) = crate::icon::decode_to_rgba(icon)?;
    if w == 0 || h == 0 {
        return Ok(DEFAULT_ICON.to_vec());
    }
    const SIDE: u32 = 192;
    let resized = crate::icon::resize_rgba_to(&rgba, w, h, SIDE, SIDE);
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, SIDE, SIDE);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| anyhow!("PNG 编码失败：{e}"))?;
        writer
            .write_image_data(&resized)
            .map_err(|e| anyhow!("PNG 写入失败：{e}"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 二进制 AndroidManifest.xml 改写（只替换字符串池里的字符串，索引不变）
// ---------------------------------------------------------------------------

/// 在 manifest 字符串池中替换 `android:label` 与 `package` 两个字段。
fn patch_manifest(tpl: &[u8], label: &str, package: &str) -> Result<Vec<u8>> {
    let pool_off = 8usize;
    if tpl.len() < pool_off + 28 {
        bail!("manifest 模板损坏");
    }
    let pool_type = u16::from_le_bytes([tpl[pool_off], tpl[pool_off + 1]]);
    if pool_type != 0x0001 {
        bail!("manifest 模板缺少字符串池");
    }
    let pool_size = u32::from_le_bytes(tpl[pool_off + 4..pool_off + 8].try_into().unwrap()) as usize;
    let count = u32::from_le_bytes(tpl[pool_off + 8..pool_off + 12].try_into().unwrap()) as usize;
    let style_count = u32::from_le_bytes(tpl[pool_off + 12..pool_off + 16].try_into().unwrap()) as usize;
    let flags = u32::from_le_bytes(tpl[pool_off + 16..pool_off + 20].try_into().unwrap());
    let strings_start = u32::from_le_bytes(tpl[pool_off + 20..pool_off + 24].try_into().unwrap()) as usize;
    let utf8 = flags & 0x100 != 0;

    // 解码所有字符串
    let base = pool_off + strings_start;
    let mut strings: Vec<String> = Vec::with_capacity(count);
    for i in 0..count {
        let o = u32::from_le_bytes(
            tpl[pool_off + 28 + i * 4..pool_off + 28 + i * 4 + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let p = base + o;
        if utf8 {
            let (len, p2) = read_utf8_len(tpl, p);
            let (_, p3) = read_utf8_len(tpl, p2);
            strings.push(String::from_utf8_lossy(&tpl[p3..p3 + len]).into_owned());
        } else {
            let n = u16::from_le_bytes([tpl[p], tpl[p + 1]]) as usize;
            let start = p + 2;
            let s: String = String::from_utf16_lossy(
                &(0..n)
                    .map(|k| {
                        u16::from_le_bytes([
                            tpl[start + k * 2],
                            tpl[start + k * 2 + 1],
                        ])
                    })
                    .collect::<Vec<u16>>(),
            );
            strings.push(s);
        }
    }

    let mut replaced = strings.clone();
    // 通过遍历 chunk 找到 android:label 与 package 的取值字符串下标
    let mut label_idx: Option<usize> = None;
    let mut pkg_idx: Option<usize> = None;
    {
        let mut pos = pool_off + pool_size;
        while pos + 8 <= tpl.len() {
            let ctype = u16::from_le_bytes([tpl[pos], tpl[pos + 1]]);
            let hsize = u16::from_le_bytes([tpl[pos + 2], tpl[pos + 3]]) as usize;
            let csize = u32::from_le_bytes(tpl[pos + 4..pos + 8].try_into().unwrap()) as usize;
            if csize == 0 {
                break;
            }
            if ctype == 0x0102 && pos + 16 + hsize <= tpl.len() {
                let name_idx =
                    u32::from_le_bytes(tpl[pos + 20..pos + 24].try_into().unwrap()) as usize;
                let attr_start = u16::from_le_bytes([tpl[pos + 24], tpl[pos + 25]]) as usize;
                let attr_size = u16::from_le_bytes([tpl[pos + 26], tpl[pos + 27]]) as usize;
                let attr_count = u16::from_le_bytes([tpl[pos + 28], tpl[pos + 29]]) as usize;
                let elem_name = strings.get(name_idx).cloned().unwrap_or_default();
                let base_attr = pos + 16 + attr_start;
                for k in 0..attr_count {
                    let a = base_attr + k * attr_size;
                    if a + 20 > tpl.len() {
                        break;
                    }
                    let a_name = u32::from_le_bytes(tpl[a + 4..a + 8].try_into().unwrap()) as usize;
                    let vtype = tpl[a + 15];
                    let vdata = u32::from_le_bytes(tpl[a + 16..a + 20].try_into().unwrap()) as usize;
                    let attr_name = strings.get(a_name).cloned().unwrap_or_default();
                    if vtype == 0x03 {
                        if elem_name == "manifest" && attr_name == "package" && pkg_idx.is_none() {
                            pkg_idx = Some(vdata);
                        }
                        if elem_name == "application" && attr_name == "label" && label_idx.is_none() {
                            label_idx = Some(vdata);
                        }
                    }
                }
            }
            pos += csize;
        }
    }
    let label_idx = label_idx.ok_or_else(|| anyhow!("manifest 模板中找不到 android:label"))?;
    let pkg_idx = pkg_idx.ok_or_else(|| anyhow!("manifest 模板中找不到 package"))?;
    if label_idx >= replaced.len() || pkg_idx >= replaced.len() {
        bail!("manifest 模板字符串下标越界");
    }
    replaced[label_idx] = label.to_string();
    replaced[pkg_idx] = package.to_string();

    // 重建字符串数据区（统一按 UTF-16 写，flags 置 0）
    let mut data: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(replaced.len());
    for s in &replaced {
        offsets.push(data.len() as u32);
        let units: Vec<u16> = s.encode_utf16().collect();
        data.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for u in units {
            data.extend_from_slice(&u.to_le_bytes());
        }
    }
    // 4 字节对齐
    while data.len() % 4 != 0 {
        data.push(0);
    }

    let new_strings_start = 28 + offsets.len() * 4 + style_count * 4;
    let new_pool_size = new_strings_start + data.len();
    let mut pool = Vec::with_capacity(new_pool_size);
    pool.extend_from_slice(&1u16.to_le_bytes()); // type
    pool.extend_from_slice(&28u16.to_le_bytes()); // header size
    pool.extend_from_slice(&(new_pool_size as u32).to_le_bytes());
    pool.extend_from_slice(&(offsets.len() as u32).to_le_bytes());
    pool.extend_from_slice(&(style_count as u32).to_le_bytes());
    pool.extend_from_slice(&0u32.to_le_bytes()); // flags = UTF-16
    pool.extend_from_slice(&(new_strings_start as u32).to_le_bytes());
    pool.extend_from_slice(&if style_count == 0 {
        0u32.to_le_bytes()
    } else {
        (new_pool_size as u32).to_le_bytes()
    });
    for o in &offsets {
        pool.extend_from_slice(&o.to_le_bytes());
    }
    // 样式区（本模板为空，占位保留）
    if style_count > 0 {
        pool.extend(std::iter::repeat(0u8).take(style_count * 4));
    }
    pool.extend_from_slice(&data);

    // 拼接：文件头 + 新字符串池 + 其余 chunk
    let rest_start = pool_off + pool_size;
    let mut out: Vec<u8> = Vec::with_capacity(rest_start + pool.len() + tpl.len() - rest_start);
    out.extend_from_slice(&tpl[..pool_off]);
    out.extend_from_slice(&pool);
    if rest_start < tpl.len() {
        out.extend_from_slice(&tpl[rest_start..]);
    }
    let total = out.len() as u32;
    out[4..8].copy_from_slice(&total.to_le_bytes());
    Ok(out)
}

fn read_utf8_len(b: &[u8], p: usize) -> (usize, usize) {
    let v = b[p];
    if v & 0x80 == 0 {
        (v as usize, p + 1)
    } else {
        (((v & 0x7f) as usize) << 8 | ((b[p + 1] as usize) & 0xff), p + 2)
    }
}

// ---------------------------------------------------------------------------
// v2 签名
// ---------------------------------------------------------------------------

fn sign_in_place(tmp: &Path, output: &Path) -> Result<()> {
    use rsa::pkcs8::EncodePublicKey;
    let mut f = File::open(tmp).context("打开临时 APK 失败")?;
    let len = f.metadata()?.len();

    // 解析 EOCD（必要时含 Zip64 EOCD）
    let (eocd_off, cd_off, cd_size, cd_section_end, zip64_cd_off) = locate_eocd(&mut f, len)?;

    // 计算签名块长度（与内容无关，先算出来才能改写 EOCD 中的偏移）
    let (cert_der, key) = load_signing_material()?;
    let spki_der = key
        .to_public_key()
        .to_public_key_der()
        .map_err(|e| anyhow!("导出公钥失败：{e}"))?
        .as_bytes()
        .to_vec();
    let sig_block_len = estimate_sig_block_len(&cert_der, &spki_der)?;

    let new_cd_off = cd_off + sig_block_len as u64;

    // 三段内容摘要
    let mut f = File::open(tmp)?;
    let d1 = chunked_digest(&mut f, 0, cd_off)?;
    let d2 = chunked_digest(&mut f, cd_off, cd_size)?;

    // EOCD 段：签名摘要按 apksig 规范计算「CD 偏移 = 签名块起点」的版本，
    // 而最终写入文件的 EOCD 需要把 CD 偏移改成真实位置（签名块之后）。
    f.seek(SeekFrom::Start(cd_section_end))?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail)?;
    // 摘要用：保持原始 CD 偏移（即签名块插入点）
    let d3 = chunked_digest_bytes(&tail)?;
    // 输出用：把 CD 偏移改成插入签名块后的真实位置
    let mut tail_patched = tail.clone();
    let eocd_rel = (eocd_off - cd_section_end) as usize;
    if zip64_cd_off.is_some() {
        // tail 的起点就是 Zip64 EOCD 记录本身，CD 偏移字段在 +48
        tail_patched[48..56].copy_from_slice(&new_cd_off.to_le_bytes());
        // locator 紧邻 EOCD 之前，其 zip64 EOCD 指向也要随插入整体后移
        let loc = eocd_rel - 20;
        let new_z64_off = cd_section_end + sig_block_len as u64;
        tail_patched[loc + 8..loc + 16].copy_from_slice(&new_z64_off.to_le_bytes());
    }
    let cur = u32::from_le_bytes(tail_patched[eocd_rel + 16..eocd_rel + 20].try_into().unwrap());
    if cur != 0xFFFF_FFFF {
        tail_patched[eocd_rel + 16..eocd_rel + 20]
            .copy_from_slice(&(new_cd_off as u32).to_le_bytes());
    }

    let block = build_signature_block(&cert_der, &spki_der, &key, &[d1, d2, d3])?;
    if block.len() as u64 != sig_block_len {
        bail!(
            "签名块长度与预估不一致：预估 {} 实际 {}",
            sig_block_len,
            block.len()
        );
    }

    // 写出：entries | sig block | CD | zip64 EOCD | locator | EOCD(改写)
    let mut out = BufWriter::new(
        File::create(output).with_context(|| format!("创建 APK 失败：{}", output.display()))?,
    );
    copy_range(&mut f, 0, cd_off, &mut out)?;
    out.write_all(&block)?;
    copy_range(&mut f, cd_off, cd_size, &mut out)?;
    out.write_all(&tail_patched)?;
    out.flush().ok();
    Ok(())
}

/// 定位 EOCD，返回 (EOCD 偏移, 中央目录偏移, 中央目录大小, 尾部段起始, zip64 EOCD 相对尾部起点)
fn locate_eocd(
    f: &mut File,
    len: u64,
) -> Result<(u64, u64, u64, u64, Option<u64>)> {
    let tail_len = std::cmp::min(len, 64 * 1024 + 32);
    f.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail)?;
    let mut eocd_idx = None;
    for i in (0..tail.len().saturating_sub(21)).rev() {
        if tail[i] == b'P' && tail[i + 1] == b'K' && tail[i + 2] == 0x05 && tail[i + 3] == 0x06 {
            eocd_idx = Some(i);
            break;
        }
    }
    let eocd_rel = eocd_idx.ok_or_else(|| anyhow!("APK 缺少 EOCD"))?;
    let eocd_off = len - tail_len + eocd_rel as u64;
    let cd_size32 = u32::from_le_bytes(tail[eocd_rel + 12..eocd_rel + 16].try_into().unwrap());
    let cd_off32 = u32::from_le_bytes(tail[eocd_rel + 16..eocd_rel + 20].try_into().unwrap());

    if cd_off32 == 0xFFFF_FFFF || cd_size32 == 0xFFFF_FFFF {
        // Zip64：locator 紧邻 EOCD 之前
        if eocd_rel < 20 {
            bail!("Zip64 EOCD locator 缺失");
        }
        let loc = eocd_rel - 20;
        if !(tail[loc] == b'P' && tail[loc + 1] == b'K' && tail[loc + 2] == 6 && tail[loc + 3] == 7) {
            bail!("Zip64 EOCD locator 签名不符");
        }
        let z64_off = u64::from_le_bytes(tail[loc + 8..loc + 16].try_into().unwrap());
        f.seek(SeekFrom::Start(z64_off))?;
        let mut z = [0u8; 56];
        f.read_exact(&mut z)?;
        if !(z[0] == b'P' && z[1] == b'K' && z[2] == 6 && z[3] == 6) {
            bail!("Zip64 EOCD 签名不符");
        }
        let cd_size = u64::from_le_bytes(z[40..48].try_into().unwrap());
        let cd_off = u64::from_le_bytes(z[48..56].try_into().unwrap());
        Ok((
            eocd_off,
            cd_off,
            cd_size,
            z64_off,
            Some(z64_off - (len - tail_len)),
        ))
    } else {
        Ok((
            eocd_off,
            cd_off32 as u64,
            cd_size32 as u64,
            eocd_off,
            None,
        ))
    }
}

fn copy_range<W: Write>(f: &mut File, start: u64, len: u64, out: &mut W) -> Result<()> {
    f.seek(SeekFrom::Start(start))?;
    let mut remaining = len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let n = std::cmp::min(remaining, buf.len() as u64) as usize;
        f.read_exact(&mut buf[..n])?;
        out.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}

/// APK v2 chunked 摘要：1MB 分块，每块 SHA256(0xa5 || len || data)，再 SHA256(0x5a || count || chunks)
fn chunked_digest(f: &mut File, start: u64, len: u64) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(start))?;
    let mut remaining = len;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let n = std::cmp::min(remaining, buf.len() as u64) as usize;
        f.read_exact(&mut buf[..n])?;
        chunks.push(sha256(&{
            let mut v = Vec::with_capacity(5 + n);
            v.push(0xa5);
            v.extend_from_slice(&(n as u32).to_le_bytes());
            v.extend_from_slice(&buf[..n]);
            v
        }));
        remaining -= n as u64;
    }
    let mut top: Vec<u8> = Vec::new();
    top.push(0x5a);
    top.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for c in &chunks {
        top.extend_from_slice(c);
    }
    Ok(sha256(&top))
}

fn chunked_digest_bytes(data: &[u8]) -> Result<Vec<u8>> {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for c in data.chunks(1024 * 1024) {
        let mut v = Vec::with_capacity(5 + c.len());
        v.push(0xa5);
        v.extend_from_slice(&(c.len() as u32).to_le_bytes());
        v.extend_from_slice(c);
        chunks.push(sha256(&v));
    }
    let mut top: Vec<u8> = Vec::new();
    top.push(0x5a);
    top.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for c in &chunks {
        top.extend_from_slice(c);
    }
    Ok(sha256(&top))
}

fn sha256(b: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().to_vec()
}

fn load_signing_material() -> Result<(Vec<u8>, rsa::RsaPrivateKey)> {
    use rsa::pkcs8::DecodePrivateKey;
    let cert_der = pem_to_der(CERT_PEM)?;
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(std::str::from_utf8(KEY_PEM)?)
        .map_err(|e| anyhow!("解析私钥失败：{e}"))?;
    Ok((cert_der, key))
}

fn pem_to_der(pem: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(pem).context("PEM 文本无效")?;
    let mut b64 = String::new();
    let mut in_body = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("-----BEGIN") {
            in_body = true;
            continue;
        }
        if t.starts_with("-----END") {
            break;
        }
        if in_body {
            b64.push_str(t);
        }
    }
    Ok(base64_decode(&b64)?)
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    const TBL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4 + 3);
    let mut buf = [0u8; 4];
    let mut n = 0;
    for ch in s.bytes() {
        if ch == b'=' || ch.is_ascii_whitespace() {
            continue;
        }
        let v = TBL
            .iter()
            .position(|&c| c == ch)
            .ok_or_else(|| anyhow!("base64 含非法字符"))? as u8;
        buf[n] = v;
        n += 1;
        if n == 4 {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
            out.push((buf[2] << 6) | buf[3]);
            n = 0;
        }
    }
    match n {
        2 => out.push((buf[0] << 2) | (buf[1] >> 4)),
        3 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        _ => {}
    }
    Ok(out)
}

/// 预计算签名块长度（结构固定，与摘要值无关）。
fn estimate_sig_block_len(cert_der: &[u8], spki: &[u8]) -> Result<u64> {
    // 用全零摘要占位，长度与真实签名一致
    let zero = vec![0u8; 32];
    let block = build_signature_block(cert_der, spki, &load_signing_material()?.1, &[zero.clone(), zero.clone(), zero])?;
    Ok(block.len() as u64)
}

fn u32lp(v: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
    out.extend_from_slice(v);
}

/// 构造 APK Signature Scheme v2 签名块。
fn build_signature_block(
    cert_der: &[u8],
    spki: &[u8],
    key: &rsa::RsaPrivateKey,
    digests: &[Vec<u8>],
) -> Result<Vec<u8>> {
    const SIG_ALG: u32 = 0x0103; // RSASSA-PKCS1-v1_5 + SHA-256

    // signed data: digests
    let mut digest_list = Vec::new();
    digest_list.extend_from_slice(&(digests.len() as u32).to_le_bytes());
    for d in digests {
        let mut item = Vec::new();
        item.extend_from_slice(&SIG_ALG.to_le_bytes());
        u32lp(d, &mut item);
        u32lp(&item, &mut digest_list);
    }
    // certificates
    let mut certs = Vec::new();
    certs.extend_from_slice(&1u32.to_le_bytes());
    u32lp(cert_der, &mut certs);
    // additional attributes = 空
    let attrs = 0u32.to_le_bytes();

    let mut signed_data = Vec::new();
    u32lp(&digest_list, &mut signed_data);
    u32lp(&certs, &mut signed_data);
    u32lp(&attrs, &mut signed_data);

    // 签名
    use rsa::signature::{SignatureEncoding, Signer};
    let sk = rsa::pkcs1v15::SigningKey::<Sha256>::new(key.clone());
    let sig = sk.sign(&signed_data).to_vec();

    let mut sigs = Vec::new();
    sigs.extend_from_slice(&1u32.to_le_bytes());
    let mut sig_item = Vec::new();
    sig_item.extend_from_slice(&SIG_ALG.to_le_bytes());
    u32lp(&sig, &mut sig_item);
    u32lp(&sig_item, &mut sigs);

    let mut value = Vec::new();
    u32lp(&signed_data, &mut value);
    u32lp(&sigs, &mut value);
    u32lp(spki, &mut value);

    // 单个 ID-value 对：0x7109871a = APK Signature Scheme v2
    let mut pairs = Vec::new();
    pairs.extend_from_slice(&((value.len() + 8) as u64).to_le_bytes());
    pairs.extend_from_slice(&0x7109_871au32.to_le_bytes());
    pairs.extend_from_slice(&value);

    let size = (pairs.len() + 8 + 16) as u64;
    let mut block = Vec::new();
    block.extend_from_slice(&size.to_le_bytes());
    block.extend_from_slice(&pairs);
    block.extend_from_slice(&size.to_le_bytes());
    block.extend_from_slice(b"APK Sig Block 42");
    Ok(block)
}
