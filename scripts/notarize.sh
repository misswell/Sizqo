#!/bin/bash
# Sizqo 分发管线（构建 + 签名 + 公证 + 装订）
# 镜像已签名公证的 OctoPilot 方案：Team U8U443D7ZL / hardened runtime /
# disable-library-validation / notarized + stapled / arm64-only。
#
# 用法：
#   bash scripts/notarize.sh                  # 构建 + 签名 + 公证 + 装订
#   SIGN_ONLY=1 bash scripts/notarize.sh      # 构建 + 签名，跳过公证
# 环境变量（可选覆盖）：
#   SIGNING_IDENTITY          指定 codesign 签名身份；默认自动检测 Developer ID Application
#   NOTARY_PROFILE            notarytool 钥匙串凭据 profile 名，默认 sizqo-notary。
#                            注意：这个 profile **不一定存在**（本机就没有）。脚本会先用
#                            `notarytool history` 实测；不存在就自动改走下面的 API key 路线。
#   APPLE_API_KEY_PATH / APPLE_API_KEY_ID / APPLE_API_ISSUER
#                            App Store Connect API key（.p8 路径 + Key ID + Issuer UUID）。
#                            无交互 shell（CI / agent）写不进钥匙串时用这条，
#                            PEM 由调用方持有，脚本不复制、不负责清理。
#   APPLE_ID / APPLE_APP_SPECIFIC_PASSWORD / APPLE_TEAM_ID   第三条兜底路线
#   SKIP_BUILD=1              跳过 cargo tauri build，复用已有 .app
#   VERBOSE=1                 输出签名详情

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TAURI_DIR="$PROJECT_DIR/src-tauri"
APP_NAME="Sizqo"
APP="$TAURI_DIR/target/release/bundle/macos/$APP_NAME.app"
BUNDLE_ID="com.misswell.sizqo"
ENTITLEMENTS="$TAURI_DIR/entitlements.plist"
DEFAULT_TEAM_ID="U8U443D7ZL"

log()  { echo "==> $*"; }
ok()   { echo "    ✓ $*"; }
fail() { echo "✗ $*" >&2; exit 1; }

# ---------- 0. 前置检查 ----------
[ -f "$ENTITLEMENTS" ] || fail "找不到 entitlements：$ENTITLEMENTS"

# 检测 Developer ID 签名身份
if [ -z "${SIGNING_IDENTITY:-}" ]; then
  SIGNING_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1 || true)"
fi
[ -n "${SIGNING_IDENTITY:-}" ] || {
  echo "✗ 未检测到 Developer ID Application 签名身份，可能存在以下原因之一。" >&2
  echo "" >&2
  echo "  [1] 本机尚未创建/安装 Developer ID Application 证书（最常见）。" >&2
  echo "      → 付费 Apple Developer Program 成员可在 Xcode：" >&2
  echo "        Settings → Accounts → 选你的 Apple ID → Manage Certificates…" >&2
  echo "        → 左下 『+』 → Developer ID Application" >&2
  echo "        或 developer.apple.com → Certificates, Identifiers & Profiles 新建并双击安装。" >&2
  echo "      → 验证：security find-identity -v -p codesigning 应看到 'Developer ID Application: ... (U8U443D7ZL)'" >&2
  echo "" >&2
  echo "  [2] 钥匙串搜索列表已损坏（本机的表现：security list-keychains 报 'errSecParam'）。" >&2
  echo "      → 一次性修复（重置为默认并重启）：" >&2
  echo "        defaults delete com.apple.security DLDBSearchList" >&2
  echo "        然后注销/重启，再用 Keychain Access 确认 login 钥匙串可见。" >&2
  echo "" >&2
  echo "  [3] 仍无法定位，可显式传入签名身份名后运行：" >&2
  echo "      SIGNING_IDENTITY='Developer ID Application: Your Name (U8U443D7ZL)' NOTARY_PROFILE=sizqo-notary $0" >&2
  echo "" >&2
  exit 2
}
log "签名身份：$SIGNING_IDENTITY"

# ---------- 1. 构建 ----------
# 在线更新：钥匙串存有 Sizqo 独立签名私钥（sizqo-updater-privkey）时才启用，
# 构建合并 tauri.conf.updater.json（独立公钥 + GitHub latest.json 源），
# 并设置 SIZQO_UPDATER_CONFIGURED=1（与 lib.rs 的插件注册门禁一致）。
# 没有密钥就按"更新禁用"构建 —— 不注册插件，不出更新产物。
UPDATER_PRIVKEY="$(security find-generic-password -s sizqo-updater-privkey -w 2>/dev/null | base64 -d 2>/dev/null || true)"
UPDATER_KEYPW="$(security find-generic-password -s sizqo-updater-key-password -w 2>/dev/null || true)"
UPDATER_MERGE="$TAURI_DIR/tauri.conf.updater.json"
UPDATER_ENABLED=0
BUILD_ENV=()
if [ -n "$UPDATER_PRIVKEY" ] && [ -f "$UPDATER_MERGE" ]; then
  UPDATER_ENABLED=1
  BUILD_ENV=(SIZQO_UPDATER_CONFIGURED=1 TAURI_SIGNING_PRIVATE_KEY="$UPDATER_PRIVKEY" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$UPDATER_KEYPW")
  log "在线更新：已启用（Sizqo 独立密钥，签名更新产物 + latest.json）"
else
  log "在线更新：钥匙串无签名私钥，按禁用构建（更新插件不注册）"
fi
if [ "${SKIP_BUILD:-0}" != "1" ]; then
  log "cargo tauri build --bundles app（release, arm64, 不由 Tauri 自动签名；DMG 由本脚本后续用 hdiutil 生成）"
  # 只构建 .app，避开 Tauri 自带的 create-dmg 步骤（该步骤在新版 create-dmg 上会因参数不匹配而失败）
  ( cd "$TAURI_DIR" && env "${BUILD_ENV[@]}" cargo tauri build --bundles app ${UPDATER_ENABLED:+--config "$UPDATER_MERGE"} ) || fail "cargo tauri build 失败"
fi
[ -d "$APP" ] || fail "构建产物不存在：${APP}（请先 SKIP_BUILD=0 跑一次构建）"

# ---------- 1.5 在线更新产物 ----------
# 更新包必须在内置工具拷进 .app、签名并装订之后才生成：构建期 createUpdaterArtifacts
# 产出的 tar.gz 只含 Info.plist / icon.icns / 主程序（Resources 里既没有 bin/lib，
# 也没有 video），拿它发 feed 等于让在线更新装出一个没有压缩引擎的 app。
# 构建期那份只是副产品，第 10 步会用装订完成的 app 覆盖它。

# ---------- 2. 打包内置工具到 .app ----------
log "复制内置 CLI 工具和动态库到 .app/Contents/Resources"
RES_DIR="$APP/Contents/Resources"
BIN_DIR="$RES_DIR/bin"
LIB_DIR="$RES_DIR/lib"
mkdir -p "$BIN_DIR" "$LIB_DIR"
SRC_BIN="$TAURI_DIR/resources/bin"
SRC_LIB="$TAURI_DIR/resources/lib"
[ -d "$SRC_BIN" ] || fail "缺少内置工具源：$SRC_BIN"
[ -d "$SRC_LIB" ] || fail "缺少内置库源：$SRC_LIB"
tool_count=0
for tool in "$SRC_BIN"/*; do
  [ -f "$tool" ] || continue
  cp "$tool" "$BIN_DIR/$(basename "$tool")"
  chmod 755 "$BIN_DIR/$(basename "$tool")"
  tool_count=$((tool_count+1))
done
ok "$tool_count 个 CLI 工具"
lib_count=0
for lib in "$SRC_LIB"/*.dylib; do
  [ -f "$lib" ] || continue
  cp "$lib" "$LIB_DIR/$(basename "$lib")"
  chmod 755 "$LIB_DIR/$(basename "$lib")"
  lib_count=$((lib_count+1))
done
ok "$lib_count 个动态库"

# 视频引擎资源：ffmpeg / ffprobe 及其动态库放在 Resources/video/，
# 运行时由 video::process::command 按 <exe>/../Resources/video/bin 查找。
if [ -d "$TAURI_DIR/resources/video/bin" ]; then
  VIDEO_BIN="$RES_DIR/video/bin"
  VIDEO_LIB="$RES_DIR/video/lib"
  mkdir -p "$VIDEO_BIN" "$VIDEO_LIB"
  video_count=0
  for tool in "$TAURI_DIR/resources/video/bin"/*; do
    [ -f "$tool" ] || continue
    cp "$tool" "$VIDEO_BIN/$(basename "$tool")"
    chmod 755 "$VIDEO_BIN/$(basename "$tool")"
    video_count=$((video_count+1))
  done
  for lib in "$TAURI_DIR/resources/video/lib"/*.dylib; do
    [ -f "$lib" ] || continue
    cp "$lib" "$VIDEO_LIB/$(basename "$lib")"
    chmod 755 "$VIDEO_LIB/$(basename "$lib")"
  done
  ok "视频引擎资源：$video_count 个工具"
else
  fail "缺少视频引擎资源：$TAURI_DIR/resources/video/bin（先跑 scripts/collect_macos_tools.sh）"
fi

# ---------- 3. 签名（先叶子后根：dylib → bin → 主程序） ----------
CS_OPTS=( --force --options runtime --entitlements "$ENTITLEMENTS" --sign "$SIGNING_IDENTITY" )
[ "${VERBOSE:-0}" = "1" ] && CS_OPTS=( --force --options runtime --entitlements "$ENTITLEMENTS" --sign "$SIGNING_IDENTITY" --verbose=4 )

log "签名内置动态库"
for lib in "$LIB_DIR"/*.dylib "$RES_DIR/video/lib"/*.dylib; do
  [ -f "$lib" ] || continue
  codesign "${CS_OPTS[@]}" "$lib" >/dev/null 2>&1 || codesign "${CS_OPTS[@]}" "$lib"
  ok "$(basename "$lib")"
done

log "签名内置 CLI 工具"
for bin in "$BIN_DIR"/* "$RES_DIR/video/bin"/*; do
  [ -f "$bin" ] || continue
  codesign "${CS_OPTS[@]}" "$bin" >/dev/null 2>&1 || codesign "${CS_OPTS[@]}" "$bin"
  ok "$(basename "$bin")"
done

log "签名主程序 $APP_NAME.app"
# 如有辅助程序/框架，先逐个签名（本应用无 Contents/Frameworks、无 Helper）
find "$APP/Contents" -type d \( -name "*.app" -o -name "*.framework" \) -prune -exec sh -c '
  for d; do codesign --force --options runtime --entitlements "$0" --sign "$1" "$d" >/dev/null 2>&1; done
' "$ENTITLEMENTS" "$SIGNING_IDENTITY" {} + 2>/dev/null || true

codesign "${CS_OPTS[@]}" "$APP" >/dev/null 2>&1 || codesign "${CS_OPTS[@]}" "$APP"
ok "$APP_NAME.app 已签名（hardened runtime + entitlements）"

# ---------- 4. 校验签名 ----------
log "校验签名与 Gatekeeper"
codesign --verify --strict --verbose=2 "$APP" 2>&1 | sed 's/^/    /'
if spctl -a -vvv -t exec "$APP" 2>&1 | tee /tmp/sizqo_spctl.log | grep -q "accepted"; then
  ok "Gatekeeper 接受"
else
  echo "    ⚠ spctl 评估（未公证前可能显示 rejected，公证装订后会通过）：" >&2
  sed 's/^/      /' /tmp/sizqo_spctl.log
fi

echo ""
log "应用大小：$(du -sh "$APP" | awk '{print $1}')"

# ---------- 5. 仅签名模式 ----------
if [ "${SIGN_ONLY:-0}" = "1" ]; then
  echo ""
  log "SIGN_ONLY=1：跳过公证，签名产物已生成：$APP"
  exit 0
fi

# ---------- 6. 公证凭据前置检查 ----------
# 三条路线，按顺序取第一条可用的。默认仍是钥匙串 profile；API key 路线存在的原因是
# 非交互 shell（CI / agent）里 store-credentials 会撞 "User interaction is not allowed"，
# 写不进钥匙串，但不公证又不行——Apple 的 --key/--key-id/--issuer 本来就允许直接给
# PEM 路径，不经过任何凭据存储。PEM 文件由调用方持有并负责清理（脚本里不落地副本，
# 免得和后面的 trap 抢 EXIT 清理时机，把密钥留在 /tmp）。
TEAM_ID="${APPLE_TEAM_ID:-$DEFAULT_TEAM_ID}"
NOTARY_PROFILE="${NOTARY_PROFILE:-sizqo-notary}"
NOTARY_ARGS=()
if [ -n "${NOTARY_PROFILE:-}" ] && xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
  NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
  log "公证凭据：钥匙串 profile $NOTARY_PROFILE"
elif [ -n "${APPLE_API_KEY_PATH:-}" ] && [ -n "${APPLE_API_KEY_ID:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ]; then
  [ -f "$APPLE_API_KEY_PATH" ] || fail "APPLE_API_KEY_PATH 指向的文件不存在：$APPLE_API_KEY_PATH"
  NOTARY_ARGS=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER")
  log "公证凭据：App Store Connect API key（不写钥匙串）"
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]; then
  NOTARY_ARGS=(--apple-id "$APPLE_ID" --team-id "$TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD")
  log "公证凭据：Apple ID + app-specific password"
else
  echo "✗ 没有可用的公证凭据（三选一）：" >&2
  echo "  [1] 钥匙串 profile（默认名 $NOTARY_PROFILE）：" >&2
  echo "        xcrun notarytool store-credentials <profile> --apple-id <你的 Apple ID> --team-id $DEFAULT_TEAM_ID" >&2
  echo "      已存在的 profile 可用 NOTARY_PROFILE=<name> 指定；无交互 shell 里这条会写不进钥匙串。" >&2
  echo "  [2] App Store Connect API key（推荐用于 CI / 无交互 shell）：" >&2
  echo "        APPLE_API_KEY_PATH=<.p8 路径> APPLE_API_KEY_ID=<Key ID> APPLE_API_ISSUER=<Issuer UUID>" >&2
  echo "  [3] APPLE_ID + APPLE_APP_SPECIFIC_PASSWORD（+ APPLE_TEAM_ID）" >&2
  echo "  本地已签名产物保留在：$APP" >&2
  exit 3
fi

# ---------- 7. 制作 DMG ----------
STAGING="$(mktemp -d -t sizqo_dmg)"
trap 'rm -rf "$STAGING"' EXIT
cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
DMG="$TAURI_DIR/target/release/bundle/macos/$APP_NAME-$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")-macos.dmg"
log "制作 DMG：$(basename "$DMG")"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGING" -ov -format UDZO "$DMG" >/dev/null
codesign "${CS_OPTS[@]}" --no-strict "$DMG" >/dev/null 2>&1 || true
ok "$(basename "$DMG")"

# ---------- 8. 提交公证 ----------
log "提交 Apple 公证（notarytool --wait）"
xcrun notarytool submit "$DMG" "${NOTARY_ARGS[@]}" --wait

# ---------- 9. 装订公证票据 ----------
log "装订公证票据"
xcrun stapler staple "$APP"
xcrun stapler staple "$DMG"
ok "stapled"

log "校验装订"
xcrun stapler validate "$APP" 2>&1 | sed 's/^/    /'
spctl -a -vvv -t exec "$APP" 2>&1 | sed 's/^/    /'

# ---------- 10. 用装订完成的 app 生成更新包 ----------
if [ "$UPDATER_ENABLED" = "1" ]; then
  APP_VERSION="$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")"
  BUNDLE_DIR="$TAURI_DIR/target/release/bundle/macos"
  TGZ="$BUNDLE_DIR/$APP_NAME.app.tar.gz"
  SIG="$TGZ.sig"
  LATEST="$BUNDLE_DIR/latest.json"
  log "重新打包更新产物：$(basename "$TGZ")（含内置工具与视频引擎）"
  tar -czf "$TGZ" -C "$(dirname "$APP")" "$APP_NAME.app"
  env TAURI_SIGNING_PRIVATE_KEY="$UPDATER_PRIVKEY" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$UPDATER_KEYPW" \
    cargo tauri signer sign "$TGZ" >/dev/null || fail "更新包签名失败"
  [ -f "$SIG" ] || fail "更新包签名缺失：$SIG"
  python3 - "$APP_VERSION" "$TGZ" "$SIG" "$LATEST" << 'PYEOF'
import datetime, json, os, sys
version, tgz, sig, out = sys.argv[1:5]
signature = open(sig).read().strip()
feed = {
    "version": version,
    "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "notes": f"Sizqo {version}",
    "platforms": {
        "darwin-aarch64": {
            "signature": signature,
            "url": f"https://github.com/misswell/Sizqo/releases/download/v{version}/Sizqo.app.tar.gz",
        }
    },
}
open(out, "w").write(json.dumps(feed, ensure_ascii=False, indent=2) + "\n")
print(f"latest.json -> {version} | archive {os.path.getsize(tgz)} bytes")
PYEOF
  ok "更新包 $(du -h "$TGZ" | awk '{print $1}')，latest.json 已生成"
fi

echo ""
log "🎉 分发完成！"
echo "    .app : $APP"
echo "    .dmg : $DMG"
echo "    大小 : $(du -h "$DMG" | awk '{print $1}')"
