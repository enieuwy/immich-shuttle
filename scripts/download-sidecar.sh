#!/usr/bin/env bash
set -euo pipefail

IMMICH_GO_VERSION="0.31.0"

TARGET="${1:-}"
if [[ "${TARGET}" == "--target" ]]; then
  TARGET="${2:-}"
fi

if [[ -z "${TARGET}" ]]; then
  OS="$(uname -s)"
  ARCH="$(uname -m)"
  if [[ "${OS}" == "Darwin" && "${ARCH}" == "arm64" ]]; then
    TARGET="aarch64-apple-darwin"
  elif [[ "${OS}" == "Darwin" && ("${ARCH}" == "x86_64" || "${ARCH}" == "amd64") ]]; then
    TARGET="x86_64-apple-darwin"
  elif [[ "${OS}" == "Linux" && ("${ARCH}" == "x86_64" || "${ARCH}" == "amd64") ]]; then
    TARGET="x86_64-unknown-linux-gnu"
  elif [[ "${OS}" == MINGW64_NT* || "${OS}" == MSYS_NT* || "${OS}" == CYGWIN_NT* ]]; then
    TARGET="x86_64-pc-windows-msvc"
  else
    echo "Unsupported platform: ${OS} ${ARCH}" >&2
    exit 1
  fi
fi

case "${TARGET}" in
  aarch64-apple-darwin)
    RELEASE_ASSET="immich-go_Darwin_arm64.tar.gz"
    OUTPUT_NAME="immich-go-aarch64-apple-darwin"
    ;;
  x86_64-apple-darwin)
    RELEASE_ASSET="immich-go_Darwin_x86_64.tar.gz"
    OUTPUT_NAME="immich-go-x86_64-apple-darwin"
    ;;
  x86_64-unknown-linux-gnu)
    RELEASE_ASSET="immich-go_Linux_x86_64.tar.gz"
    OUTPUT_NAME="immich-go-x86_64-unknown-linux-gnu"
    ;;
  x86_64-pc-windows-msvc)
    RELEASE_ASSET="immich-go_Windows_x86_64.zip"
    OUTPUT_NAME="immich-go-x86_64-pc-windows-msvc.exe"
    ;;
  *)
    echo "Unsupported target triple: ${TARGET}" >&2
    exit 1
    ;;
esac

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
BIN_DIR="${ROOT_DIR}/src-tauri/binaries"
mkdir -p "${BIN_DIR}"

URL="https://github.com/simulot/immich-go/releases/download/v${IMMICH_GO_VERSION}/${RELEASE_ASSET}"
DEST="${BIN_DIR}/${OUTPUT_NAME}"
TMP_FILE="${BIN_DIR}/${RELEASE_ASSET}"
TMP_DIR="${BIN_DIR}/.tmp"

rm -rf "${TMP_DIR}"
mkdir -p "${TMP_DIR}"

curl -fL "${URL}" -o "${TMP_FILE}"

if [[ "${RELEASE_ASSET}" == *.tar.gz ]]; then
  tar -xzf "${TMP_FILE}" -C "${TMP_DIR}"
elif [[ "${RELEASE_ASSET}" == *.zip ]]; then
  unzip -o "${TMP_FILE}" -d "${TMP_DIR}" >/dev/null
else
  echo "Unsupported asset archive format: ${RELEASE_ASSET}" >&2
  exit 1
fi

SOURCE_BIN="${TMP_DIR}/immich-go"
if [[ "${TARGET}" == "x86_64-pc-windows-msvc" ]]; then
  SOURCE_BIN="${TMP_DIR}/immich-go.exe"
fi

if [[ ! -f "${SOURCE_BIN}" ]]; then
  echo "Could not find extracted binary at ${SOURCE_BIN}" >&2
  exit 1
fi

cp "${SOURCE_BIN}" "${DEST}"
chmod +x "${DEST}"
rm -rf "${TMP_DIR}" "${TMP_FILE}"

echo "Downloaded ${DEST}"
