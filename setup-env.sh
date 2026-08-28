#!/bin/sh
# rebuilds the modslut build environment after a sandbox wipe.
# usage: sh /mnt/agents/output/modslut/setup-env.sh
set -e
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh
  sh /tmp/rustup.sh -y --default-toolchain stable --target x86_64-pc-windows-gnu >/dev/null 2>&1
fi
T="$HOME/toolchains/llvm-mingw-20260616-ucrt-ubuntu-22.04-x86_64"
if [ ! -x "$T/bin/x86_64-w64-mingw32-gcc" ]; then
  cd /tmp
  curl -sL --retry 3 -o llvm-mingw.tar.xz https://github.com/mstorsjo/llvm-mingw/releases/download/20260616/llvm-mingw-20260616-ucrt-ubuntu-22.04-x86_64.tar.xz
  mkdir -p "$HOME/toolchains"
  tar -xf llvm-mingw.tar.xz -C "$HOME/toolchains"
  mkdir -p "$HOME/toolchains/libgcc-shim"
  cp "$T"/lib/clang/*/lib/windows/libclang_rt.builtins-x86_64.a "$HOME/toolchains/libgcc-shim/libgcc.a"
  cp "$T/x86_64-w64-mingw32/lib/libunwind.a" "$HOME/toolchains/libgcc-shim/libgcc_eh.a"
fi
echo "toolchain ready"
echo "build:  export PATH=\"\$HOME/.cargo/bin:\$HOME/toolchains/llvm-mingw-20260616-ucrt-ubuntu-22.04-x86_64/bin:\$PATH\" CARGO_TARGET_DIR=/tmp/modslut-target"
echo "linux:  cargo build --release            (in /mnt/agents/output/modslut)"
echo "win:    cargo build --release --target x86_64-pc-windows-gnu"
