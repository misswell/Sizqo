#!/bin/bash
# 投递前的架构门：目录里每个会被 exec 的 Mach-O 都必须带目标架构的切片。
#
# scripts/collect_macos_tools.sh 的架构校验发生在「采集时」，用的是 `uname -m`；
# 这一步发生在「装进 .app 之后」，用的是这个 job 的目标架构。两个时点分开才有意义：
# 换 runner 镜像、加缓存、跳过采集脚本、手改资源路径 —— 任何一种都会让包里躺着
# 本机跑不动的引擎，而 src/binfmt.rs 那道运行时的门只会把它报成「不可用」，
# 用户看到的是视频/图片功能凭空缺失，日志里一个错都没有。
#
# 第三种目标 `universal`：合并包里被 exec 的那几个文件（主程序 + 图片工具 + 视频引擎）
# 必须**同时**含 arm64 与 x86_64。这个目标只交给调用方指定的目录，不交给 `lib/`：
# 两条腿的 homebrew 不同点小版本时，依赖库会按名字分家（libcrypto.4 vs .3），
# 那时单架构 dylib 是合法产物，硬要求双切片只会把发布作业永久钉红。
set -euo pipefail

DIR="${1:?待检查目录 required}"
ARCH="${2:?目标架构 required}"

case "${ARCH}" in
  arm64|x86_64) REQUIRED="${ARCH}"; WANT_DESC="${ARCH}" ;;
  universal)    REQUIRED="arm64 x86_64"; WANT_DESC="arm64 与 x86_64" ;;
  *) echo "✗ 目标架构只能是 arm64 / x86_64 / universal，收到 ${ARCH}" >&2; exit 2 ;;
esac
[ -d "${DIR}" ] || { echo "✗ 目录不存在：${DIR}" >&2; exit 2; }

checked=0
while IFS= read -r -d '' file; do
  file "${file}" | grep -q 'Mach-O' || continue
  archs="$(lipo -archs "${file}")"
  missing=""
  for want in ${REQUIRED}; do
    # 按家族匹配而不是按字面：lipo 会把 Apple Silicon 的产物报成 arm64e（本机 /bin/ls
    # 就是 `x86_64 arm64e arm64e.x1`），只认字面 "arm64" 的门会把合法的双切片包判红。
    case "${want}" in
      arm64)  accept="arm64 arm64e" ;;
      x86_64) accept="x86_64 x86_64h" ;;
      *)      accept="${want}" ;;
    esac
    found=""
    for a in ${accept}; do
      case " ${archs} " in
        *" ${a} "*) found="${a}"; break ;;
      esac
    done
    [ -n "${found}" ] || missing="${missing} ${want}"
  done
  if [ -n "${missing}" ]; then
    printf '✗  %s 的切片 [%s] 里缺目标架构%s\n' "${file#"${DIR}/"}" "${archs}" "${missing}" >&2
    exit 1
  fi
  printf 'ok   %s → %s\n' "${file#"${DIR}/"}" "${archs}"
  checked=$((checked + 1))
done < <(find "${DIR}" -type f -print0)

# 一个 Mach-O 都没数到就是这条门根本没跑：空目录和通过长得一模一样。
[ "${checked}" -gt 0 ] || {
  echo "✗ ${DIR} 里没有任何 Mach-O 文件，架构门没有可检查的对象" >&2
  exit 1
}
echo "架构门通过：${DIR} 里 ${checked} 个 Mach-O 都含 ${WANT_DESC}"
