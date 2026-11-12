#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

# Wyss DBS Localizer installer script (Linux, macOS)
# Installs the release under
#   $INSTALL_DIR/lib/dbsloc/{bin,resources}
# with launchers for every executable in $INSTALL_DIR/bin. The executables locate
# resources/ next to their own directory, so the release layout is kept intact.
# Run from an extracted release archive it installs that folder (no download);
# otherwise it downloads the latest release archive. -u removes an installation.

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Defaults
INSTALL_DIR="${DBSLOC_INSTALL_DIR:-$HOME/.local}"

RELEASE_TAG="${DBSLOC_RELEASE_TAG:-}"
LOCAL_ARCHIVE=""
UNINSTALL=0
EXPLICIT_SOURCE=0

# Parse arguments
while getopts ":d:v:a:u" opt; do
  case $opt in
    d) INSTALL_DIR="$OPTARG" ;;
    v) RELEASE_TAG="$OPTARG"; EXPLICIT_SOURCE=1 ;;
    a) LOCAL_ARCHIVE="$OPTARG"; EXPLICIT_SOURCE=1 ;;
    u) UNINSTALL=1 ;;
    *) echo "Usage: $0 [-d install_dir] [-v tag] [-a archive] [-u]" >&2; exit 1 ;;
  esac
done

case "$INSTALL_DIR" in
  /*) ;;
  *) INSTALL_DIR="$PWD/$INSTALL_DIR" ;;
esac

BIN_DIR="$INSTALL_DIR/bin"
LIB_DIR="$INSTALL_DIR/lib/dbsloc"
RESOURCES_DIR="$LIB_DIR/resources"
EXECUTABLES="dbs-localizer dbsloc"
STALE_EXECUTABLES="dbsloc_scene3d dbsloc_checkreg dbsloc_gencheckregpair"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_FILE="$DATA_HOME/applications/dbs-localizer.desktop"
ICON_FILE="$DATA_HOME/icons/hicolor/256x256/apps/dbs-localizer.png"

# Folder holding this script when it is run from a file (not piped into bash)
SCRIPT_DIR=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

remove_launchers() {
  for exe in "$@"; do
    launcher="$BIN_DIR/$exe"
    if [ -L "$launcher" ]; then
      case "$(readlink "$launcher")" in
        "$LIB_DIR"/*) rm -f "$launcher"; echo "Removed $launcher" ;;
      esac
    elif [ -f "$launcher" ] && grep -qF "exec \"$LIB_DIR/" "$launcher" 2>/dev/null; then
      rm -f "$launcher"
      echo "Removed $launcher"
    fi
  done
}

# Uninstall: remove the installed release, our launchers, the menu entry and the icon
if [ "$UNINSTALL" = 1 ]; then
  remove_launchers $EXECUTABLES $STALE_EXECUTABLES
  if [ -e "$LIB_DIR" ]; then
    rm -rf "$LIB_DIR"
    echo "Removed $LIB_DIR"
  fi
  if [ -f "$DESKTOP_FILE" ] && grep -qF "$BIN_DIR/dbs-localizer" "$DESKTOP_FILE"; then
    for f in "$DESKTOP_FILE" "$ICON_FILE"; do
      if [ -e "$f" ]; then
        rm -f "$f"
        echo "Removed $f"
      fi
    done
  fi
  if command -v update-desktop-database >/dev/null 2>&1 && [ -d "$(dirname "$DESKTOP_FILE")" ]; then
    update-desktop-database "$(dirname "$DESKTOP_FILE")" >/dev/null 2>&1 || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -d "$DATA_HOME/icons/hicolor" ]; then
    gtk-update-icon-cache -q -t "$DATA_HOME/icons/hicolor" >/dev/null 2>&1 || true
  fi
  echo -e "${GREEN}Uninstalled.${NC}"
  exit 0
fi

# Detect platform and CPU architecture
PLATFORM="unknown"
ARCH="$(uname -m)"
case "$(uname -s)" in
  Linux*)
    PLATFORM="linux"
    case "$ARCH" in
      x86_64|amd64) ;;
      *)
        echo -e "${RED}Unsupported CPU architecture on Linux: $ARCH (the release is built for x86_64 only).${NC}" >&2
        exit 1 ;;
    esac ;;
  Darwin*)
    PLATFORM="mac"
    case "$ARCH" in
      arm64|aarch64) ;;
      *)
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" != "1" ]; then
          echo -e "${RED}Unsupported CPU architecture on macOS: $ARCH (the release is built for Apple silicon, arm64, only).${NC}" >&2
          exit 1
        fi ;;
    esac ;;
  CYGWIN*|MINGW*|MSYS*)
    echo -e "${RED}Windows is not installed with this script.${NC}" >&2
    echo "Download dbs-localizer-setup.exe from https://github.com/WyssCenter/dbs-localizer/releases" >&2
    echo "and run it; no administrator rights needed." >&2
    exit 1 ;;
  *) echo -e "${RED}Unsupported platform: $(uname -s)${NC}" >&2; exit 1 ;;
esac

echo -e "${GREEN}Detected platform: $PLATFORM ($ARCH)${NC}"

# Download, verify and extract Wyss DBS Localizer
ARCHIVE_NAME="dbsloc-$PLATFORM.tar.gz"
REPO="WyssCenter/dbs-localizer"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/dbsloc-install.XXXXXX")"
STAGE_DIR=""
cleanup() {
  rm -rf "$WORK_DIR"
  if [ -n "$STAGE_DIR" ]; then
    rm -rf "$STAGE_DIR"
  fi
}
trap cleanup EXIT

download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    echo -e "${RED}Neither curl nor wget found. Cannot download.${NC}" >&2
    exit 1
  fi
}

# Prints the API URL of the release asset called $2 found in the release JSON file $1
asset_api_url() {
  tr -d '\n' < "$1" \
    | grep -oE '"(url|name)": *"[^"]*"' \
    | awk -v want="$2" '
        /^"url"/ { u = $0; sub(/^"url": *"/, "", u); sub(/"$/, "", u); cur = (u ~ /\/releases\/assets\/[0-9]+$/) ? u : ""; next }
        /^"name"/ { n = $0; sub(/^"name": *"/, "", n); sub(/"$/, "", n); if (cur != "" && n == want) { print cur; exit } cur = "" }'
}

# curl with the token passed through stdin so it never shows up in a process list
api_curl() {
  printf 'header = "Authorization: Bearer %s"\n' "$API_TOKEN" | curl -fsSL -K - \
    -H "X-GitHub-Api-Version: 2022-11-28" "$@"
}

download_with_gh() {
  gh release download ${RELEASE_TAG:+"$RELEASE_TAG"} --repo "$REPO" \
    --pattern "$ARCHIVE_NAME" --pattern "$ARCHIVE_NAME.sha256" --dir "$WORK_DIR"
}

download_with_token() {
  local api="https://api.github.com/repos/$REPO/releases"
  if [ -n "$RELEASE_TAG" ]; then api="$api/tags/$RELEASE_TAG"; else api="$api/latest"; fi
  api_curl -H "Accept: application/vnd.github+json" "$api" -o "$WORK_DIR/release.json" || return 1
  local name url
  for name in "$ARCHIVE_NAME" "$ARCHIVE_NAME.sha256"; do
    url="$(asset_api_url "$WORK_DIR/release.json" "$name")"
    [ -n "$url" ] || return 1
    api_curl -H "Accept: application/octet-stream" "$url" -o "$WORK_DIR/$name" || return 1
  done
}

download_anonymously() {
  local base="https://github.com/$REPO/releases/latest/download"
  if [ -n "$RELEASE_TAG" ]; then base="https://github.com/$REPO/releases/download/$RELEASE_TAG"; fi
  download "$base/$ARCHIVE_NAME" "$WORK_DIR/$ARCHIVE_NAME" \
    && download "$base/$ARCHIVE_NAME.sha256" "$WORK_DIR/$ARCHIVE_NAME.sha256"
}

USE_LOCAL_DIR=0
if [ "$EXPLICIT_SOURCE" = 0 ] && [ -n "$SCRIPT_DIR" ] \
   && [ -f "$SCRIPT_DIR/bin/dbsloc" ] && [ -d "$SCRIPT_DIR/resources" ]; then
  USE_LOCAL_DIR=1
fi

if [ "$USE_LOCAL_DIR" = 1 ]; then
  echo -e "${YELLOW}Installing from $SCRIPT_DIR${NC}"
elif [ -n "$LOCAL_ARCHIVE" ]; then
  if [ ! -f "$LOCAL_ARCHIVE" ] || [ ! -f "$LOCAL_ARCHIVE.sha256" ]; then
    echo -e "${RED}Archive or checksum file not found: $LOCAL_ARCHIVE and $LOCAL_ARCHIVE.sha256 must both exist.${NC}" >&2
    exit 1
  fi
  cp "$LOCAL_ARCHIVE" "$WORK_DIR/$ARCHIVE_NAME"
  cp "$LOCAL_ARCHIVE.sha256" "$WORK_DIR/$ARCHIVE_NAME.sha256"
else
  echo -e "${YELLOW}Downloading Wyss DBS Localizer for $PLATFORM...${NC}"
  API_TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    DOWNLOAD_FUNCTION=download_with_gh
  elif [ -n "$API_TOKEN" ] && command -v curl >/dev/null 2>&1; then
    DOWNLOAD_FUNCTION=download_with_token
  else
    DOWNLOAD_FUNCTION=download_anonymously
  fi
  if ! "$DOWNLOAD_FUNCTION"; then
    echo -e "${RED}Download failed. Check the network connection, or download $ARCHIVE_NAME from${NC}" >&2
    echo "  https://github.com/$REPO/releases in a web browser, then:" >&2
    echo "  tar -xzf $ARCHIVE_NAME && ./dbsloc-$PLATFORM/install.sh" >&2
    exit 1
  fi
fi

if [ "$USE_LOCAL_DIR" = 0 ]; then
  echo -e "${YELLOW}Verifying the checksum...${NC}"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$WORK_DIR" && sha256sum -c "$ARCHIVE_NAME.sha256")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$WORK_DIR" && shasum -a 256 -c "$ARCHIVE_NAME.sha256")
  else
    echo -e "${RED}Neither sha256sum nor shasum found. Cannot verify the download.${NC}" >&2
    exit 1
  fi
fi

mkdir -p "$INSTALL_DIR/lib" "$BIN_DIR"
STAGE_DIR="$(mktemp -d "$INSTALL_DIR/lib/.dbsloc-new.XXXXXX")"
NEW_DIR="$STAGE_DIR/dbsloc-$PLATFORM"
if [ "$USE_LOCAL_DIR" = 1 ]; then
  echo -e "${YELLOW}Copying...${NC}"
  cp -a "$SCRIPT_DIR" "$NEW_DIR"
else
  echo -e "${YELLOW}Extracting...${NC}"
  tar -xzf "$WORK_DIR/$ARCHIVE_NAME" -C "$STAGE_DIR"
fi
if [ ! -d "$NEW_DIR/bin" ] || [ ! -d "$NEW_DIR/resources" ]; then
  echo -e "${RED}Unexpected archive layout: expected dbsloc-$PLATFORM/{bin,resources}${NC}" >&2
  exit 1
fi
chmod +x "$NEW_DIR/bin/"* || true
chmod +x "$NEW_DIR/resources/ants/$PLATFORM/"* "$NEW_DIR/resources/dcm2niix/$PLATFORM/"* 2>/dev/null || true

# Verify the new release before replacing the installed one
echo -e "${YELLOW}Verifying the new release...${NC}"
for exe in dbsloc dbs-localizer; do
  if [ ! -x "$NEW_DIR/bin/$exe" ]; then
    echo -e "${RED}❌ $exe missing from the release archive${NC}" >&2
    exit 1
  fi
done
if ! "$NEW_DIR/bin/dbsloc" --version; then
  echo -e "${RED}❌ dbsloc from the release archive does not run on this machine${NC}" >&2
  exit 1
fi
if [ "$PLATFORM" = "linux" ] && command -v ldd >/dev/null 2>&1; then
  MISSING="$(ldd "$NEW_DIR/bin/dbs-localizer" 2>/dev/null | awk '/not found/ {print $1}' | sort -u | tr '\n' ' ' || true)"
  if [ -n "$MISSING" ]; then
    echo -e "${YELLOW}⚠ dbs-localizer (the GUI) needs shared libraries this system does not have: $MISSING${NC}" >&2
    if command -v pacman >/dev/null 2>&1; then
      echo -e "${YELLOW}  Arch Linux: sudo pacman -S --needed gtk3 vulkan-icd-loader libxkbcommon fontconfig${NC}" >&2
    elif command -v apt-get >/dev/null 2>&1; then
      echo -e "${YELLOW}  Debian / Ubuntu: sudo apt-get install libgtk-3-0 libvulkan1 libxkbcommon0 fontconfig${NC}" >&2
    elif command -v dnf >/dev/null 2>&1; then
      echo -e "${YELLOW}  Fedora: sudo dnf install gtk3 vulkan-loader libxkbcommon fontconfig${NC}" >&2
    fi
    echo -e "${YELLOW}  The command line tool dbsloc works without them; the GUI will not start until they are installed.${NC}" >&2
  fi
fi
NEW_ANT_REG="$NEW_DIR/resources/ants/$PLATFORM/antsRegistration"
if [ ! -x "$NEW_ANT_REG" ] || ! "$NEW_ANT_REG" --version >/dev/null 2>&1; then
  echo -e "${RED}❌ Bundled ANTs missing or not runnable: $NEW_ANT_REG${NC}" >&2
  exit 1
fi
NEW_TEMPLATES="$("$NEW_DIR/bin/dbsloc" template list 2>/dev/null || true)"
if ! grep -q bundled <<<"$NEW_TEMPLATES"; then
  echo -e "${RED}❌ The shipped template was not found in the release archive${NC}" >&2
  exit 1
fi

# Replace the installed release layout as a whole
echo -e "${YELLOW}Installing to $LIB_DIR...${NC}"
OLD_DIR=""
if [ -e "$LIB_DIR" ]; then
  OLD_DIR="$STAGE_DIR/previous"
  mv "$LIB_DIR" "$OLD_DIR"
fi
if ! mv "$NEW_DIR" "$LIB_DIR"; then
  if [ -n "$OLD_DIR" ]; then
    mv "$OLD_DIR" "$LIB_DIR"
  fi
  echo -e "${RED}❌ Could not move the new release into $LIB_DIR${NC}" >&2
  exit 1
fi

# Launchers in $BIN_DIR. On Linux std::env::current_exe() reads /proc/self/exe, so a
# symlink resolves to the real file and resources/ is found. On macOS it returns the
# path the process was started with (symlinks unresolved), so a wrapper script that
# execs the real binary is installed instead.
echo -e "${YELLOW}Linking executables into $BIN_DIR...${NC}"
remove_launchers $STALE_EXECUTABLES
for exe in $EXECUTABLES; do
  target="$LIB_DIR/bin/$exe"
  if [ ! -f "$target" ]; then
    echo -e "${YELLOW}⚠ $exe not in this release; skipped${NC}"
    continue
  fi
  rm -f "$BIN_DIR/$exe"
  if [ "$PLATFORM" = "linux" ]; then
    ln -s "$target" "$BIN_DIR/$exe"
  else
    printf '#!/bin/sh\nexec "%s" "$@"\n' "$target" > "$BIN_DIR/$exe"
    chmod +x "$BIN_DIR/$exe"
  fi
done

# Verify installation
echo -e "${YELLOW}Verifying installation...${NC}"
for exe in dbsloc dbs-localizer; do
  if [ -x "$BIN_DIR/$exe" ]; then
    echo -e "${GREEN}✅ $exe installed: $BIN_DIR/$exe${NC}"
  else
    echo -e "${RED}❌ $exe not installed${NC}" >&2
    exit 1
  fi
done
"$BIN_DIR/dbsloc" --version

# Verify bundled ANTs
ANT_REG="$RESOURCES_DIR/ants/$PLATFORM/antsRegistration"
if [ -f "$ANT_REG" ] && [ -x "$ANT_REG" ]; then
  echo -e "${GREEN}✅ Bundled ANTs found: $ANT_REG${NC}"
  ANT_VERSION="$("$ANT_REG" --version 2>/dev/null || true)"
  echo "${ANT_VERSION%%$'\n'*}"
else
  echo -e "${RED}❌ Bundled ANTs not found${NC}" >&2
  exit 1
fi

# Verify bundled dcm2niix (DICOM import)
DCM2NIIX="$RESOURCES_DIR/dcm2niix/$PLATFORM/dcm2niix"
if [ -f "$DCM2NIIX" ]; then
  echo -e "${GREEN}✅ Bundled dcm2niix found: $DCM2NIIX${NC}"
  "$DCM2NIIX" -v 2>/dev/null | tail -1 || true
else
  echo -e "${YELLOW}⚠ Bundled dcm2niix not found; DICOM import needs dcm2niix on PATH${NC}"
fi

# Verify the shipped template is found from the installed location
INSTALLED_TEMPLATES="$("$BIN_DIR/dbsloc" template list 2>/dev/null || true)"
if grep -q bundled <<<"$INSTALLED_TEMPLATES"; then
  echo -e "${GREEN}✅ Shipped template found${NC}"
else
  echo -e "${RED}❌ The shipped template was not found from $BIN_DIR/dbsloc${NC}" >&2
  exit 1
fi

# Desktop integration
if [ "$PLATFORM" = "linux" ]; then
  TEMPLATE="$LIB_DIR/dbs-localizer.desktop.in"
  ICON_SRC="$LIB_DIR/dbs-localizer.png"
  if [ -f "$TEMPLATE" ] && [ -f "$ICON_SRC" ]; then
    mkdir -p "$(dirname "$DESKTOP_FILE")" "$(dirname "$ICON_FILE")"
    EXEC_PATH="$BIN_DIR/dbs-localizer"
    case "$EXEC_PATH" in
      *" "*) EXEC_PATH="\"$EXEC_PATH\"" ;;
    esac
    ENTRY="$(cat "$TEMPLATE")"
    printf '%s\n' "${ENTRY//@EXEC@/"$EXEC_PATH"}" > "$DESKTOP_FILE"
    cp "$ICON_SRC" "$ICON_FILE"
    chmod 644 "$ICON_FILE"
    if command -v update-desktop-database >/dev/null 2>&1; then
      update-desktop-database "$(dirname "$DESKTOP_FILE")" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
      gtk-update-icon-cache -q -t "$DATA_HOME/icons/hicolor" >/dev/null 2>&1 || true
    fi
    echo -e "${GREEN}✅ Menu entry installed: $DESKTOP_FILE${NC}"
  else
    echo -e "${YELLOW}⚠ Menu entry skipped: icon or desktop template not in this release${NC}"
  fi
elif [ "$PLATFORM" = "mac" ] && command -v xattr >/dev/null 2>&1; then
  xattr -dr com.apple.quarantine "$LIB_DIR" >/dev/null 2>&1 || true
fi

echo -e "\n${GREEN}Installation complete!${NC}"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo -e "${YELLOW}$BIN_DIR is not in your PATH. Add it with:${NC}"
    echo "  export PATH=\"$BIN_DIR:\$PATH\"   (put this line in ~/.profile or your shell's rc file)" ;;
esac
echo -e "\n${YELLOW}Run GUI:${NC}"
echo "  dbs-localizer"
echo -e "${YELLOW}Run CLI:${NC}"
echo "  dbsloc --help"
