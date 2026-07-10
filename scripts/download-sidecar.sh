#!/usr/bin/env bash
set -euo pipefail

IMMICH_GO_VERSION="0.32.0"

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
    EXPECTED_SHA="0c1ce06932a21a738d9adaf2ddafdbf654113fc2791d31b6d2ea0044928e2f43"
    ;;
  x86_64-apple-darwin)
    RELEASE_ASSET="immich-go_Darwin_x86_64.tar.gz"
    OUTPUT_NAME="immich-go-x86_64-apple-darwin"
    EXPECTED_SHA="35b9456d36c0bbb7d5087d08622b087f849f4f6d9631b15737a22cef39e5a314"
    ;;
  x86_64-unknown-linux-gnu)
    RELEASE_ASSET="immich-go_Linux_x86_64.tar.gz"
    OUTPUT_NAME="immich-go-x86_64-unknown-linux-gnu"
    EXPECTED_SHA="6e2ad86bafdadb9466d6515de7cb882726c0aea1a21d51164dff361d7d480a97"
    ;;
  x86_64-pc-windows-msvc)
    RELEASE_ASSET="immich-go_Windows_x86_64.zip"
    OUTPUT_NAME="immich-go-x86_64-pc-windows-msvc.exe"
    EXPECTED_SHA="37326f95d6654f7f67a5fde3263e96aa8f9cf288dcbb9a2164aafae0aaa144af"
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

# Verify the downloaded archive against the SHA-256 pinned in THIS repo, not a
# checksums.txt fetched from the same mutable upstream release. If an attacker
# replaced the release asset they could also replace checksums.txt, so trusting
# it would defeat the check; the repo-pinned value is the trust root. Bump both
# IMMICH_GO_VERSION and the EXPECTED_SHA values together when upgrading.
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA="$(sha256sum "${TMP_FILE}" | awk '{ print $1 }')"
else
  ACTUAL_SHA="$(shasum -a 256 "${TMP_FILE}" | awk '{ print $1 }')"
fi

if [[ "${EXPECTED_SHA}" != "${ACTUAL_SHA}" ]]; then
  echo "Checksum mismatch for ${RELEASE_ASSET}" >&2
  echo "  expected ${EXPECTED_SHA} (pinned in scripts/download-sidecar.sh)" >&2
  echo "  actual   ${ACTUAL_SHA}" >&2
  exit 1
fi
echo "Verified checksum for ${RELEASE_ASSET} against repo-pinned SHA-256"

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
