#!/usr/bin/env bash
# Install zoder from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/mockupcode/zoder/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/mockupcode/zoder/main/install.sh | bash -s 0.1.1
#
set -euo pipefail

REPO="${ZODER_REPO:-mockupcode/zoder}"
BIN_DIR="${ZODER_BIN_DIR:-$HOME/.zoder/bin}"
TARGET="${1:-}"

if [[ -n "$TARGET" && ! "$TARGET" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._]+)?$ ]]; then
  echo "Invalid version: $TARGET (expected X.Y.Z)" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required" >&2
  exit 1
fi

case "$(uname -s)" in
  Darwin) os="macos" ;;
  Linux) os="linux" ;;
  *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

if [ "$os" = "macos" ] && [ "$arch" = "x86_64" ]; then
  sysctl_bin="$(command -v sysctl || echo /usr/sbin/sysctl)"
  if [ "$("$sysctl_bin" -n hw.optional.arm64 2>/dev/null)" = "1" ]; then
    echo "Apple Silicon detected; installing aarch64." >&2
    arch="aarch64"
  fi
fi

platform="${os}-${arch}"
asset="zoder-${platform}"

if [ -n "$TARGET" ]; then
  version="$TARGET"
  base="https://github.com/${REPO}/releases/download/v${version}"
else
  echo "Fetching latest release..." >&2
  api="https://api.github.com/repos/${REPO}/releases/latest"
  json="$(curl -fsSL "$api")"
  version="$(printf '%s' "$json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)"
  if [ -z "$version" ]; then
    echo "No GitHub release found. Push a tag like v0.1.0 to build one." >&2
    exit 1
  fi
  base="https://github.com/${REPO}/releases/download/v${version}"
fi

echo "Installing zoder ${version} (${platform})..." >&2
mkdir -p "$BIN_DIR"
tmp="$(mktemp)"
url="${base}/${asset}"
if ! curl -fsSL -o "$tmp" "$url"; then
  echo "Download failed: $url" >&2
  echo "Need a release asset named ${asset}." >&2
  rm -f "$tmp"
  exit 1
fi

chmod +x "$tmp"
if ! "$tmp" --version >/dev/null 2>&1; then
  echo "Downloaded binary failed to run." >&2
  rm -f "$tmp"
  exit 1
fi

mv -f "$tmp" "$BIN_DIR/zoder"
echo "Installed $BIN_DIR/zoder" >&2

path_has_dir() {
  case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}

linked=""
if ! path_has_dir "$BIN_DIR"; then
  for candidate in "$HOME/.local/bin" /usr/local/bin; do
    if path_has_dir "$candidate" && [ -d "$candidate" ] && [ -w "$candidate" ]; then
      ln -sf "$BIN_DIR/zoder" "$candidate/zoder"
      linked="$candidate"
      echo "Linked $candidate/zoder" >&2
      break
    fi
  done
fi

user_shell="$(basename "${SHELL:-}")"
config_file=""
case "$user_shell" in
  bash) config_file="$HOME/.bashrc" ;;
  zsh) config_file="$HOME/.zshrc" ;;
  fish) config_file="$HOME/.config/fish/config.fish" ;;
esac

if [ -n "$config_file" ]; then
  mkdir -p "$(dirname "$config_file")"
  if [ "$user_shell" = "fish" ]; then
    new_block='# >>> zoder installer >>>
fish_add_path $HOME/.zoder/bin
# <<< zoder installer <<<'
  else
    new_block='# >>> zoder installer >>>
export PATH="$HOME/.zoder/bin:$PATH"
# <<< zoder installer <<<'
  fi
  if grep -qs "zoder installer" "$config_file" 2>/dev/null; then
    tmpcfg="$config_file.tmp.$$"
    awk '
      /# >>> zoder installer >>>/ { skip=1; next }
      /# <<< zoder installer <<</ { skip=0; next }
      !skip { print }
    ' "$config_file" >"$tmpcfg" && mv "$tmpcfg" "$config_file"
  fi
  printf '\n%s\n' "$new_block" >>"$config_file"
  echo "Updated PATH in $config_file" >&2
fi

echo "" >&2
if path_has_dir "$BIN_DIR" || [ -n "$linked" ]; then
  echo "Run 'zoder' to get started." >&2
elif [ -n "$config_file" ]; then
  echo "Restart the terminal, then run 'zoder'." >&2
else
  echo "Add $BIN_DIR to PATH, then run 'zoder':" >&2
  echo '  export PATH="$HOME/.zoder/bin:$PATH"' >&2
fi
