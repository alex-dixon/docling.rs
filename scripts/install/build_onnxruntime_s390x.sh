#!/usr/bin/env bash
# Cross-compile ONNX Runtime as a shared library for IBM Z (#504).
#
# pyke's `ort` ships prebuilt runtimes for x86_64/aarch64 only and Microsoft
# publishes no s390x release, so the `ort-load-dynamic` build of docling.rs
# dlopens a libonnxruntime.so that has to come from somewhere: this script
# builds it from source — cross-compiled on an x86_64 host with the distro
# GNU toolchain (minutes), not natively under QEMU (hours, how upstream's own
# s390x CI does it). .github/workflows/onnxruntime-s390x.yml runs it and
# publishes the result into the models release as onnxruntime-linux-s390x.tar.gz,
# which scripts/install/download_dependencies.sh fetches into
# .models/onnxruntime/ on an s390x host (or with --with-onnxruntime).
#
# Host prerequisites (Ubuntu 24.04): gcc-s390x-linux-gnu g++-s390x-linux-gnu
# cmake (>= 3.28) ninja-build python3 curl git, plus qemu-user-static if the
# result is to be run here.
#
#   scripts/install/build_onnxruntime_s390x.sh
#   ORT_TAG=v1.28.0 JOBS=8 OUT_DIR=$PWD/onnxruntime-s390x scripts/install/build_onnxruntime_s390x.sh
#
# Environment:
#   ORT_TAG              ONNX Runtime tag to build (default v1.28.0 — the version
#                        behind pyke's prebuilt binaries for ort 2.0.0-rc.13, so a
#                        dynamically loaded s390x runtime matches the linked one).
#   ORT_SRC_DIR          an existing checkout to build instead of cloning.
#   JOBS                 parallel compile jobs (default: nproc).
#   OUT_DIR              where lib/, LICENSE and VERSION land (default ./onnxruntime-s390x).
#   PROTOC               a host protoc 21.x binary (default: downloaded from the
#                        protobuf release — ONNX Runtime needs the host's protoc
#                        when cross-compiling, its own would be an s390x binary).
#   CMAKE_EXTRA_DEFINES  extra KEY=VALUE cmake definitions appended (e.g.
#                        FETCHCONTENT_SOURCE_DIR_ONNX=… to feed pre-fetched deps).
set -euo pipefail

ORT_TAG="${ORT_TAG:-v1.28.0}"
JOBS="${JOBS:-$(nproc)}"
OUT_DIR="${OUT_DIR:-$PWD/onnxruntime-s390x}"
WORK="${WORK_DIR:-$PWD/.onnxruntime-build}"
HERE="$(cd "$(dirname "$0")" && pwd)"
TOOLCHAIN="$HERE/cmake/s390x-linux-gnu.toolchain.cmake"
PROTOC_VERSION=21.12

for tool in s390x-linux-gnu-gcc s390x-linux-gnu-g++ cmake ninja python3 curl git; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required (apt-get install gcc-s390x-linux-gnu g++-s390x-linux-gnu cmake ninja-build python3 curl git)" >&2; exit 1; }
done

mkdir -p "$WORK" "$OUT_DIR"

# ONNX Runtime pins protobuf 21.12; the host needs a protoc of that series.
if [ -z "${PROTOC:-}" ]; then
  PROTOC="$WORK/protoc/bin/protoc"
  if [ ! -x "$PROTOC" ]; then
    case "$(uname -m)" in
      x86_64 | amd64) PROTOC_ARCH=x86_64 ;;
      aarch64 | arm64) PROTOC_ARCH=aarch_64 ;;
      *) echo "error: no prebuilt protoc for the host arch $(uname -m); set PROTOC" >&2; exit 1 ;;
    esac
    mkdir -p "$WORK/protoc"
    curl -fsSL -o "$WORK/protoc.zip" "https://github.com/protocolbuffers/protobuf/releases/download/v$PROTOC_VERSION/protoc-$PROTOC_VERSION-linux-$PROTOC_ARCH.zip"
    python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$WORK/protoc.zip" "$WORK/protoc"
    chmod +x "$PROTOC"
  fi
fi
"$PROTOC" --version

SRC="${ORT_SRC_DIR:-$WORK/onnxruntime}"
if [ ! -d "$SRC/.git" ]; then
  git clone --depth 1 --branch "$ORT_TAG" --recurse-submodules --shallow-submodules https://github.com/microsoft/onnxruntime.git "$SRC"
fi

# --skip_tests: no googletest binaries for the target. --skip_submodule_sync:
# the clone above is complete. onnxruntime_USE_KLEIDIAI=OFF: ARM-only, and
# build.py turns it on unconditionally. The toolchain file carries the Eigen
# flag (see there).
cd "$SRC"
# shellcheck disable=SC2086
./build.sh --config Release --build_shared_lib --parallel "$JOBS" \
  --skip_tests --skip_submodule_sync --allow_running_as_root \
  --compile_no_warning_as_error \
  --path_to_protoc_exe "$PROTOC" \
  --cmake_extra_defines "CMAKE_TOOLCHAIN_FILE=$TOOLCHAIN" onnxruntime_CROSS_COMPILING=ON \
    onnxruntime_USE_KLEIDIAI=OFF ${CMAKE_EXTRA_DEFINES:-}

BUILD="$SRC/build/Linux/Release"
rm -rf "$OUT_DIR/lib" && mkdir -p "$OUT_DIR/lib"
# libonnxruntime.so.<ver> plus the unversioned name ort dlopens; the SONAME
# keeps the versioned one resolvable from the same directory.
cp -a "$BUILD"/libonnxruntime.so* "$OUT_DIR/lib/"
s390x-linux-gnu-strip --strip-unneeded "$OUT_DIR"/lib/libonnxruntime.so.*.*.* 2>/dev/null || true
cp "$SRC/LICENSE" "$OUT_DIR/LICENSE"
tr -d '\n' < "$SRC/VERSION_NUMBER" > "$OUT_DIR/VERSION"; echo >> "$OUT_DIR/VERSION"
echo "built ONNX Runtime $(cat "$OUT_DIR/VERSION") for s390x:"
ls -la "$OUT_DIR/lib"
file "$OUT_DIR"/lib/libonnxruntime.so.*.*.* | sed 's/,.*//'
