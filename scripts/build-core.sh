#!/usr/bin/env bash
# Builds one libretro core at a pinned commit with its own Makefile (docs/cores.md).
#
#   scripts/build-core.sh <name> <repo> <commit> <platform> <out_dir>
#
# <name> is the core's cores.lock name, <platform> the Makefile's: osx, unix or win (run win
# in an MSYS2 MINGW64 shell). The library lands in <out_dir>/<name>_libretro.<dylib|so|dll>.
# The source is a shallow clone of exactly <commit> in <out_dir>/src/<name>, made the same way
# everywhere: cores put `git rev-parse --short HEAD` (and FBNeo the commit date) into
# library_version, which cores.lock pins, so the clone and the date must not vary by machine.
set -euo pipefail

if [ $# -ne 5 ]; then
  echo "usage: $0 <name> <repo> <commit> <osx|unix|win> <out_dir>" >&2
  exit 2
fi
name=$1 repo=$2 commit=$3 platform=$4 out=$5

case $platform in
  osx) ext=dylib ;;
  unix) ext=so ;;
  win) ext=dll ;;
  *) echo "platform must be osx, unix or win, not $platform" >&2; exit 2 ;;
esac

# Each core's Makefile directory and file, relative to its checkout.
args=()
case $name in
  mednafen_pce_fast) dir=. makefile=Makefile ;;
  fceumm) dir=. makefile=Makefile.libretro ;;
  genesis_plus_gx) dir=. makefile=Makefile.libretro ;;
  fbneo) dir=src/burner/libretro makefile=Makefile ;;
  *) echo "no build recipe for core $name: add one here (docs/cores.md)" >&2; exit 2 ;;
esac

src="$out/src/$name"
rm -rf "$src"
mkdir -p "$src"
git -C "$src" init -q
git -C "$src" fetch -q --depth 1 "$repo" "$commit"
git -C "$src" checkout -q FETCH_HEAD
# A fixed abbreviation, so the short hash in library_version doesn't depend on the clone.
git -C "$src" config core.abbrev 7

if [ "$name" = mednafen_pce_fast ]; then
  # Its Makefile passes the short hash (GIT_VERSION) to C++ only, but library_version is set
  # in libretro.c, so upstream builds report no commit. Pass it to C too, as the C++ flag is
  # written (make puts the variable into a shell command line).
  export CFLAGS="-DGIT_VERSION=\\\"\\ $(git -C "$src" rev-parse --short HEAD)\\\" ${CFLAGS:-}"
  if [ "$platform" = osx ]; then
    # Its bundled zlib 1.2.11 defines fdopen away when TARGET_OS_MAC is defined, which current
    # macOS SDKs do, and then clashes with <stdio.h>. Defining fdopen as itself skips that.
    export CFLAGS="-Dfdopen=fdopen $CFLAGS"
  fi
fi
if [ "$name" = fbneo ]; then
  # FBNeo's Makefile dates the build with GNU `date -d` (missing on macOS) in local time: pass
  # the commit's author date in UTC instead, so every platform reports the same version.
  args+=("GIT_DATE=$(TZ=UTC git -C "$src" log -1 --format=%ad --date=format-local:%y%m%d)")
fi

if [ "$platform" = osx ]; then
  # The oldest macOS the build runs on, which otherwise is the build machine's SDK (Makefiles
  # that pass -mmacosx-version-min themselves keep theirs). 11.0 is the first arm64 macOS.
  export MACOSX_DEPLOYMENT_TARGET=11.0
fi

if [ "$platform" = win ]; then
  # Link MinGW's runtime into the DLL: the Makefiles pass -static-libgcc -static-libstdc++,
  # but a static libstdc++ (posix thread model) still imports libwinpthread-1.dll (FBNeo
  # does). -static links every library it can statically; Windows' own stay imports. The
  # Makefiles append to LDFLAGS, so the environment's value comes first.
  export LDFLAGS="-static ${LDFLAGS:-}"
fi

jobs=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)
make -C "$src/$dir" -f "$makefile" -j"$jobs" platform="$platform" ${args[@]+"${args[@]}"}
lib="$out/${name}_libretro.$ext"
cp "$src/$dir/${name}_libretro.$ext" "$lib"

if [ "$platform" = win ]; then
  # The DLL must load on a Windows machine without MinGW: it may import Windows system DLLs
  # only, not MinGW's runtime (libgcc_s_seh-1, libstdc++-6, libwinpthread-1), which a load
  # finds on PATH on a CI runner or in an MSYS2 shell but not elsewhere.
  system='^(kernel32|user32|gdi32|advapi32|shell32|shlwapi|ole32|oleaut32|comdlg32|comctl32|'
  system+='winmm|ws2_32|wsock32|iphlpapi|bcrypt|crypt32|secur32|version|imm32|setupapi|'
  system+='cfgmgr32|ntdll|msvcrt|ucrtbase|opengl32|dsound|dinput8|xinput1_[34]|'
  system+='xinput9_1_0|api-ms-win-[a-z0-9-]+)\.dll$'
  imports=$(objdump -p "$lib" | sed -n 's/^[[:space:]]*DLL Name: //p' | tr -d '\r')
  foreign=$(grep -viE "$system" <<<"$imports" || true)
  if [ -n "$foreign" ]; then
    echo "$lib imports DLLs that aren't part of Windows:" $foreign >&2
    echo "link them statically, or add a Windows system DLL to the list here" >&2
    exit 1
  fi
fi
echo "$lib"
