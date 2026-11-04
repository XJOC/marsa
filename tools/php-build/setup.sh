#!/usr/bin/env bash
# Downloads the pinned build tools into $WORK and checks their sha256
# (downloads.sha256): spc on every platform, and on Linux the zig toolchain.
#
#   SPC_VERSION=2.8.5 ZIG_VERSION=0.17.1 WORK=/tmp/w setup.sh
#
# Left alone, `spc doctor` installs the newest zig from ziglang.org. We put a
# pinned zig where spc looks for it ($WORK/pkgroot/<arch>-linux/zig), with the
# same wrapper scripts spc writes (src/SPC/store/pkg/Zig.php), so doctor finds
# it and does not download another.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

: "${WORK:?WORK is required}" "${SPC_VERSION:?SPC_VERSION is required}"
mkdir -p "$WORK"
cd "$WORK"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) spc_asset=spc-macos-aarch64 ;;
  Darwin-x86_64) spc_asset=spc-macos-x86_64 ;;
  Linux-x86_64) spc_asset=spc-linux-x86_64 arch=x86_64 ;;
  Linux-aarch64) spc_asset=spc-linux-aarch64 arch=aarch64 ;;
  *) echo "unsupported build machine: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

fetch "https://github.com/crazywhalecc/static-php-cli/releases/download/${SPC_VERSION}/${spc_asset}.tar.gz" "${spc_asset}.tar.gz"
check_pinned "${spc_asset}.tar.gz"
tar -xzf "${spc_asset}.tar.gz"
chmod +x spc
./spc --version

if [ "$(uname -s)" = Linux ]; then
  : "${ZIG_VERSION:?ZIG_VERSION is required on Linux}"
  zig_tar="zig-${arch}-linux-${ZIG_VERSION}.tar.xz"
  fetch "https://ziglang.org/download/${ZIG_VERSION}/${zig_tar}" "$zig_tar"
  check_pinned "$zig_tar"
  fetch "https://raw.githubusercontent.com/crazywhalecc/static-php-cli/${SPC_VERSION}/src/SPC/store/scripts/zig-cc.sh" zig-cc.sh
  check_pinned zig-cc.sh

  dir="$WORK/pkgroot/${arch}-linux/zig"
  rm -rf "$dir"
  mkdir -p "$dir"
  tar -xJf "$zig_tar" -C "$dir" --strip-components=1
  cp zig-cc.sh "$dir/zig-cc"
  sed 's/zig cc/zig c++/g' zig-cc.sh > "$dir/zig-c++"
  for tool in ar ld.lld ranlib objcopy; do
    printf '#!/usr/bin/env bash\nexec zig %s $@' "$tool" > "$dir/zig-$tool"
  done
  chmod 755 "$dir"/zig-*
  got="$("$dir/zig" version)"
  if [ "$got" != "$ZIG_VERSION" ]; then
    echo "zig version is $got, expected $ZIG_VERSION" >&2
    exit 1
  fi
  echo "zig $got in $dir"
fi
