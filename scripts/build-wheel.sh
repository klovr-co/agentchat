#!/usr/bin/env bash
# Build the abi3 wheel into dist/. Linux links with zig against glibc 2.28
# (manylinux_2_28), so the wheel installs on Debian/Ubuntu-based images.
set -euo pipefail
python -m pip install 'maturin>=1.9,<2'
if [[ "$(uname -s)" == Linux ]]; then
  python -m pip install ziglang
  maturin build --release --locked --out dist --zig --compatibility manylinux_2_28
else
  maturin build --release --locked --out dist
fi
