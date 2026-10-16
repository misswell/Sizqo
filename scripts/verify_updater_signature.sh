#!/usr/bin/env bash
# 对「已经下载到本地的更新包」做与应用内更新完全相同的验签。
#
# 为什么需要它：v0.1.3–v0.2.0 的签名是对**错误字节**算出来的（打包发生在内置引擎
# 拷进 bundle 之前），当时没有任何一步能发现。构建日志里的成功只证明"签名过某个文件"，
# 不证明"签的是线上那 60 MB"。这个脚本把判据挪到用户视角的对象上：下载什么就验什么。
#
# 验证逻辑复刻 tauri-plugin-updater 的 verify_signature（latest.json 的 signature 字段
# 先 base64 解出 minisign 文本，再 Signature::decode，最后用配置里的公钥验原始字节），
# 用的也是同一个 minisign-verify crate，避免"脚本站得住但应用仍拒绝"的口径差。
#
#   用法：bash scripts/verify_updater_signature.sh <archive> [<archive>.sig] [<pubkey.b64>]
#   缺省从 src-tauri/tauri.conf.updater.json 取公钥，sig 取 archive 同名 + ".sig"。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="${1:?archive path required}"
SIG="${2:-${ARCHIVE}.sig}"
PUB_B64="${3:-}"
PUB_TMP=""
WORK=""
# 只清自己造的东西：调用方传进来的公钥文件不能顺手删掉。
trap 'if [ -n "$WORK" ]; then rm -rf "$WORK"; fi; if [ -n "$PUB_TMP" ]; then rm -f "$PUB_TMP"; fi' EXIT

[ -f "$ARCHIVE" ] || { echo "✗ 找不到更新包：$ARCHIVE" >&2; exit 1; }
[ -f "$SIG" ] || { echo "✗ 找不到签名文件：$SIG" >&2; exit 1; }

if [ -z "$PUB_B64" ]; then
  PUB_TMP="$(mktemp "${TMPDIR:-/tmp}/sizqo-pubkey.XXXXXX")"
  PUB_B64="$PUB_TMP"
  jq -r '.plugins.updater.pubkey' "$REPO/src-tauri/tauri.conf.updater.json" > "$PUB_B64"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sizqo-sigcheck.XXXXXX")"
mkdir -p "$WORK/src"
cat > "$WORK/Cargo.toml" <<'EOF'
[package]
name = "sizqo-sigcheck"
version = "0.0.0"
edition = "2021"

[dependencies]
minisign-verify = "=0.2.5"
base64 = "=0.22.1"
EOF
cat > "$WORK/src/main.rs" <<'EOF'
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use std::path::PathBuf;

fn b64_to_string(s: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(String::from_utf8(
        base64::engine::general_purpose::STANDARD.decode(s.trim())?,
    )?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let data = std::fs::read(&a[0])?;
    let public_key = PublicKey::decode(&b64_to_string(&std::fs::read_to_string(&a[2])?)?)?;
    let signature = Signature::decode(&b64_to_string(&std::fs::read_to_string(&a[1])?)?)?;
    println!(
        "archive={} bytes={} untrusted_comment={:?}",
        a[0].display(),
        data.len(),
        signature.untrusted_comment(),
    );
    public_key.verify(&data, &signature, true)?;
    println!("OK: 这份签名的确覆盖这组字节，且由配置里的公钥所对应的私钥签发");
    Ok(())
}
EOF

echo "==> 验签（离线复用本地 crate 缓存）"
(cd "$WORK" && CARGO_NET_OFFLINE=true cargo run --offline --quiet --release -- \
  "$ARCHIVE" "$SIG" "$PUB_B64")

echo "==> 本地指纹"
shasum -a 256 "$ARCHIVE" "$SIG"
