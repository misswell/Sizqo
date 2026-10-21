#!/bin/bash
# scripts/test_sign_notarize_macos_ci.sh —— 用桩命令跑通 macOS 发布脚本
#
# 这个脚本不进公证：Apple 工具全部换成桩，真实执行的是签名、DMG 命名与校验、
# 更新包打包和失败传播。它锁住两条曾经真实发生的线上失败：
#   1. hdiutil create 把 "created: ..." 打到 stdout，被 $(...) 一并捕获，
#      DMG 路径变成两行，后续存在性检查误报（v0.3.3 发布作业）。
#   2. hdiutil create 失败必须让整个脚本以非零退出，而不是继续公证。

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SIGN_SCRIPT="${SCRIPT_UNDER_TEST:-${PROJECT_DIR}/scripts/sign_notarize_macos_ci.sh}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sizqo-release-script-test.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT

log() { echo "==> $*"; }
fail() { echo "✗ $*" >&2; exit 1; }

BIN="${WORK}/bin"
APPS="${WORK}/apps"
OUT="${WORK}/release"
mkdir -p "${BIN}" "${APPS}" "${OUT}"

make_app() {
  local app="$1"
  mkdir -p "${app}/Contents/Resources/resources/bin" "${app}/Contents/Resources/video/bin"
  printf 'stub\n' > "${app}/Contents/Resources/resources/bin/cjpeg"
  printf 'stub\n' > "${app}/Contents/Resources/video/bin/ffmpeg"
}
for arch in arm64 x86_64 universal; do
  make_app "${APPS}/${arch}/Sizqo.app"
done

write_stub() {
  local name="$1"
  cat > "${BIN}/${name}"
  chmod +x "${BIN}/${name}"
}

# 每个文件都被当成 Mach-O，架构校验交给 lipo 桩。
write_stub file <<'EOF'
#!/bin/bash
echo "$1: Mach-O 64-bit executable"
EOF

write_stub lipo <<'EOF'
#!/bin/bash
case "$1" in
  -archs) echo "x86_64 arm64" ;;
esac
exit 0
EOF

write_stub security <<'EOF'
#!/bin/bash
echo '  1) ABCDEF "Developer ID Application: Guofeng Liu (U8U443D7ZL)"'
EOF

write_stub codesign <<'EOF'
#!/bin/bash
case " $* " in
  *" -dv "*)
    echo "Authority=Developer ID Application: Guofeng Liu (U8U443D7ZL)" >&2
    echo "Timestamp=Oct 21, 2026 at 3:00:00 AM" >&2 ;;
esac
exit 0
EOF

write_stub xcrun <<'EOF'
#!/bin/bash
tool="$1"; shift
case "${tool}" in
  notarytool) echo "  status: Accepted" ;;
  stapler) : ;;
esac
exit 0
EOF

write_stub spctl <<'EOF'
#!/bin/bash
exit 0
EOF

# 成功路径的 hdiutil：像真工具一样把 "created: ..." 写给 stdout，
# 并记录 staging 目录供 attach 时放进假 App。
write_stub hdiutil <<'EOF'
#!/bin/bash
cmd="$1"; shift
case "${cmd}" in
  create)
    dmg=""; src=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -srcfolder) src="$2"; shift 2 ;;
        -volname) shift 2 ;;
        -format) shift 2 ;;
        -ov) shift ;;
        *) dmg="$1"; shift ;;
      esac
    done
    echo "created: ${dmg}"
    printf 'stub dmg\n' > "${dmg}"
    rm -rf "${dmg}.app"
    cp -R "${src}/Sizqo.app" "${dmg}.app" ;;
  attach)
    dmg=""; mnt=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -mountpoint) mnt="$2"; shift 2 ;;
        -readonly|-nobrowse|-quiet) shift ;;
        *) dmg="$1"; shift ;;
      esac
    done
    cp -R "${dmg}.app" "${mnt}/Sizqo.app" ;;
  detach) : ;;
esac
exit 0
EOF

run_release() {
  PATH="${BIN}:$PATH" \
  SIGNING_KEYCHAIN="/tmp/sizqo-stub.keychain" \
  RELEASE_TAG="v0.0.0-test" \
  APPLE_ID="stub@example.com" \
  APPLE_APP_SPECIFIC_PASSWORD="stub-password" \
  APPLE_TEAM_ID="STUBTEAM123" \
    "${SIGN_SCRIPT}" \
      "${APPS}/arm64/Sizqo.app" \
      "${APPS}/x86_64/Sizqo.app" \
      "${APPS}/universal/Sizqo.app" \
      "${OUT}"
}

log "成功路径：三个 DMG 与更新包都产出，路径没有被工具输出污染"
if ! run_release > "${WORK}/success.log" 2>&1; then
  cat "${WORK}/success.log" >&2
  fail "发布脚本在桩环境里失败了"
fi
for arch in arm64 x86_64 universal; do
  dmg="${OUT}/Sizqo_v0.0.0-test_macos_${arch}.dmg"
  [ -f "${dmg}" ] || fail "缺少 ${arch} DMG：${dmg}"
  grep -F "created: ${dmg}" "${WORK}/success.log" >/dev/null \
    || fail "hdiutil 的进度行没有出现在日志里"
done
[ -f "${OUT}/Sizqo_v0.0.0-test_macos_universal.app.tar.gz" ] \
  || fail "缺少 universal 更新包"
if grep -F "Missing DMG before notarization" "${WORK}/success.log" >/dev/null; then
  fail "DMG 路径被 hdiutil 的 stdout 污染了"
fi

log "失败路径：hdiutil 重试三次后仍失败，脚本必须非零退出且不进入公证"
FAIL_BIN="${WORK}/fail-bin"
mkdir -p "${FAIL_BIN}"
cat > "${FAIL_BIN}/hdiutil" <<'EOF'
#!/bin/bash
exit 1
EOF
chmod +x "${FAIL_BIN}/hdiutil"

set +e
PATH="${FAIL_BIN}:${BIN}:$PATH" \
SIGNING_KEYCHAIN="/tmp/sizqo-stub.keychain" \
RELEASE_TAG="v0.0.0-test" \
APPLE_ID="stub@example.com" \
APPLE_APP_SPECIFIC_PASSWORD="stub-password" \
APPLE_TEAM_ID="STUBTEAM123" \
  "${SIGN_SCRIPT}" \
    "${APPS}/arm64/Sizqo.app" \
    "${APPS}/x86_64/Sizqo.app" \
    "${APPS}/universal/Sizqo.app" \
    "${WORK}/release-fail" > "${WORK}/failure.log" 2>&1
status=$?
set -e

[ "${status}" -ne 0 ] || fail "DMG 创建失败时脚本仍然以 0 退出"
grep -F "DMG was not created" "${WORK}/failure.log" >/dev/null \
  || fail "失败日志里没有 DMG 未创建的说明"
grep -F "hdiutil create failed for arm64 (attempt 3/3)" "${WORK}/failure.log" >/dev/null \
  || fail "没有看到三次重试的记录"
if grep -F "Submit three DMGs to Apple notarization" "${WORK}/failure.log" >/dev/null; then
  fail "DMG 创建失败后仍然进入了公证阶段"
fi

log "发布脚本回归通过"
