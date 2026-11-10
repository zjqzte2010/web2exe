"""独立校验本工具产出的 APK：zip 完整性、v2 签名（含三段摘要与 RSA 验签）、
内嵌页面是否与源目录一致。不依赖 Android SDK / apksigner。

用法:
    python verify_apk.py <apk路径> [源网页目录]
"""
import hashlib
import os
import struct
import sys
import zipfile

MAGIC = b"APK Sig Block 42"
V2_ID = 0x7109871A
ALG_RSA_PKCS1V15_SHA256 = 0x0103


def chunked_sha256(data: bytes) -> bytes:
    chunks = []
    for i in range(0, len(data), 1024 * 1024):
        part = data[i:i + 1024 * 1024]
        h = hashlib.sha256()
        h.update(b"\xa5" + struct.pack("<I", len(part)) + part)
        chunks.append(h.digest())
    top = hashlib.sha256()
    top.update(b"\x5a" + struct.pack("<I", len(chunks)) + b"".join(chunks))
    return top.digest()


def lp(buf: bytes, off: int):
    n = struct.unpack("<I", buf[off:off + 4])[0]
    return buf[off + 4:off + 4 + n], off + 4 + n


def axml_strings(data: bytes):
    """解析二进制 AXML 的字符串池，返回全部字符串。"""
    pos = 8  # 跳过文件头（0x0003 XML + headerSize + size）
    while pos + 8 <= len(data):
        ctype, hsize, csize = struct.unpack("<HHI", data[pos:pos + 8])
        if ctype == 0x0001:
            cnt, style_cnt, flags, str_start, _style_start = struct.unpack(
                "<IIIII", data[pos + 8:pos + 28]
            )
            utf8 = bool(flags & 0x100)
            offsets = [struct.unpack("<I", data[pos + 28 + 4 * i:pos + 32 + 4 * i])[0]
                       for i in range(cnt)]
            base = pos + str_start
            out = []
            for o in offsets:
                p = base + o
                if utf8:
                    # u8 utf16 长度 + u8 utf8 字节数 + 数据
                    _u16len = data[p]
                    blen = data[p + 1]
                    out.append(data[p + 2:p + 2 + blen].decode("utf-8", "ignore"))
                else:
                    clen = struct.unpack("<H", data[p:p + 2])[0]
                    out.append(data[p + 2:p + 2 + 2 * clen].decode("utf-16-le", "ignore"))
            return out
        if csize == 0:
            break
        pos += csize
    return []


def main(path: str, src_dir: str | None) -> int:
    data = open(path, "rb").read()
    size = os.path.getsize(path)
    print(f"APK: {path}  ({size / 1024:.1f} KB)")
    zf = zipfile.ZipFile(path)
    bad = zf.testzip()
    print("zip 条目数:", len(zf.namelist()), "损坏条目:", bad)
    for n in zf.namelist():
        info = zf.getinfo(n)
        ct = "stored" if info.compress_type == 0 else "deflate"
        print(f"   {n}  {info.file_size} bytes  {ct}")
    if bad is not None:
        print("FAIL: zip 条目 CRC 校验失败")
        return 1

    # ---- EOCD / Zip64 ----
    eocd = data.rfind(b"PK\x05\x06")
    cd_size = struct.unpack("<I", data[eocd + 12:eocd + 16])[0]
    cd_off = struct.unpack("<I", data[eocd + 16:eocd + 20])[0]
    is_zip64 = False
    if cd_off == 0xFFFFFFFF or cd_size == 0xFFFFFFFF:
        is_zip64 = True
        loc = eocd - 20
        assert data[loc:loc + 4] == b"PK\x06\x07", "缺少 Zip64 EOCD locator"
        z64 = struct.unpack("<Q", data[loc + 8:loc + 16])[0]
        assert data[z64:z64 + 4] == b"PK\x06\x06", "Zip64 EOCD 签名不符"
        cd_size = struct.unpack("<Q", data[z64 + 40:z64 + 48])[0]
        cd_off = struct.unpack("<Q", data[z64 + 48:z64 + 56])[0]
        print("zip64: cd_off", cd_off, "cd_size", cd_size)

    # ---- APK Signing Block ----
    if data[cd_off - 16:cd_off] != MAGIC:
        print("FAIL: 未找到 APK Signing Block（未做 v2 签名）")
        return 1
    trailing = struct.unpack("<Q", data[cd_off - 24:cd_off - 16])[0]
    block_start = cd_off - (trailing + 8)
    print(f"sig block: [{block_start}, {cd_off})  长度 {cd_off - block_start}")

    pos = block_start + 8
    value = None
    while pos < cd_off - 24:
        ln = struct.unpack("<Q", data[pos:pos + 8])[0]
        pid = struct.unpack("<I", data[pos + 8:pos + 12])[0]
        if pid == V2_ID:
            value = data[pos + 12:pos + 8 + ln]
        pos += 8 + ln
    if value is None:
        print("FAIL: 签名块里没有 v2（0x7109871a）记录")
        return 1

    signed_data, _ = lp(value, 0)
    sigs_blob, p = lp(value, 4 + struct.unpack("<I", value[:4])[0])
    pubkey_blob, _ = lp(value, p)

    digests_blob, _ = lp(signed_data, 0)
    certs_blob, p2 = lp(signed_data, 4 + struct.unpack("<I", signed_data[:4])[0])

    cnt = struct.unpack("<I", certs_blob[:4])[0]
    off = 4
    certs = []
    for _ in range(cnt):
        c, off = lp(certs_blob, off)
        certs.append(c)

    digests = []
    cnt = struct.unpack("<I", digests_blob[:4])[0]
    off = 4
    for _ in range(cnt):
        item, off = lp(digests_blob, off)
        alg = struct.unpack("<I", item[:4])[0]
        digests.append((alg, lp(item, 4)[0]))

    sig_items = []
    cnt = struct.unpack("<I", sigs_blob[:4])[0]
    off = 4
    for _ in range(cnt):
        item, off = lp(sigs_blob, off)
        alg = struct.unpack("<I", item[:4])[0]
        sig_items.append((alg, lp(item, 4)[0]))

    # ---- 三段摘要 ----
    s1 = chunked_sha256(data[:block_start])
    s2 = chunked_sha256(data[cd_off:cd_off + cd_size])
    tail = bytearray(data[cd_off + cd_size:])
    if tail[:4] == b"PK\x06\x06":
        tail[48:56] = struct.pack("<Q", block_start)
        loc_rel = len(tail) - 22 - 20
        assert tail[loc_rel:loc_rel + 4] == b"PK\x06\x07", "locator 缺失"
        tail[loc_rel + 8:loc_rel + 16] = struct.pack("<Q", block_start + cd_size)
    else:
        tail[16:20] = struct.pack("<I", block_start)
    s3 = chunked_sha256(bytes(tail))

    ok_all = True
    for i, (alg, d) in enumerate(digests):
        expect = [s1, s2, s3][i] if i < 3 else None
        match = expect is not None and d == expect
        print(f"  摘要段{i + 1}（alg={alg:#06x}）匹配: {match}")
        ok_all &= match
    if not ok_all:
        print("FAIL: 摘要与内容不一致")
        return 1

    # ---- RSA 验签 ----
    try:
        from cryptography import x509
        from cryptography.hazmat.primitives import hashes, serialization
        from cryptography.hazmat.primitives.asymmetric import padding

        cert = x509.load_der_x509_certificate(certs[0])
        print("签名证书:", cert.subject.rfc4514_string())
        pub = cert.public_key()
        for alg, s in sig_items:
            if alg != ALG_RSA_PKCS1V15_SHA256:
                print("跳过非 RSA-PKCS1v15-SHA256 算法", hex(alg))
                continue
            pub.verify(s, signed_data, padding.PKCS1v15(), hashes.SHA256())
            print("RSA 验签: OK（v2 签名有效）")
        spki = pub.public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
        print("公钥与签名块内公钥一致:", spki == pubkey_blob)
    except ImportError:
        print("（未安装 cryptography，跳过 RSA 验签，仅校验摘要）")

    # ---- 内嵌页面与源目录一致性 ----
    if src_dir:
        src_index = os.path.join(src_dir, "index.html")
        if os.path.isfile(src_index):
            local = hashlib.sha256(open(src_index, "rb").read()).hexdigest()
            packed = hashlib.sha256(zf.read("assets/www/index.html")).hexdigest()
            print("assets/www/index.html 与源 index.html SHA256 一致:", local == packed)
            if local != packed:
                return 1

    # ---- manifest 关键信息 ----
    mf = zf.read("AndroidManifest.xml")
    strings = axml_strings(mf)
    import re
    pkgs = [s for s in strings if re.match(r"^com\.\w+", s)]
    print("manifest 包名:", pkgs[:1] if pkgs else "（未解析到）")
    labels = [s for s in strings if any("\u4e00" <= ch <= "\u9fff" for ch in s)]
    print("manifest 中文标签:", labels[:3])
    print("manifest 字符串数:", len(strings))
    print("VERIFY PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None))
