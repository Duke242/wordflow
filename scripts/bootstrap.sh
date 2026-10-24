#!/usr/bin/env bash
# Gets WordFlow running on a Mac that has nothing installed yet: installs the
# Xcode command line tools, Homebrew, Node, CMake and Rust, clones the repo and
# runs `npm run setup`. Safe to run again; it skips anything already installed.
#
#   curl -fsSL https://raw.githubusercontent.com/Duke242/wordflow/main/scripts/bootstrap.sh | bash
#
# Set WORDFLOW_DIR to clone somewhere other than ~/wordflow.

set -euo pipefail

REPO_URL="https://github.com/Duke242/wordflow.git"
DIR="${WORDFLOW_DIR:-$HOME/wordflow}"

step() { printf '\n\033[1m▸ %s\033[0m\n' "$1"; }
fail() { printf '\n\033[31mbootstrap: %s\033[0m\n' "$1" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

main() {
  if [ "$(uname -s)" != "Darwin" ]; then
    fail "this script is for macOS. On Windows or Linux, follow CONTRIBUTING.md, then run \`npm run setup\`."
  fi

  step "Xcode command line tools"
  if ! xcode-select -p >/dev/null 2>&1; then
    xcode-select --install || true
    echo "Finish the install in the window that opened. Waiting…"
    until xcode-select -p >/dev/null 2>&1; do sleep 5; done
  fi
  echo "installed"

  step "Homebrew"
  if ! has brew; then
    # The Homebrew installer needs sudo but won't ask for it when run non-interactively.
    echo "Homebrew needs your Mac password to install."
    sudo -v </dev/tty
    NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" </dev/null
  fi
  # Apple Silicon and Intel Macs install Homebrew in different places.
  for brew_bin in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [ -x "$brew_bin" ]; then eval "$("$brew_bin" shellenv)"; break; fi
  done
  has brew || fail "Homebrew didn't install. See https://brew.sh"
  echo "$(brew --version | head -1)"

  step "Node and CMake"
  if ! has node || [ "$(node -p 'process.versions.node.split(".")[0]')" -lt 22 ]; then
    brew install node </dev/null
  fi
  has cmake || brew install cmake </dev/null
  echo "node $(node --version), $(cmake --version | head -1)"

  step "Rust"
  if ! has cargo && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  fi
  export PATH="$HOME/.cargo/bin:$PATH"
  cargo --version

  step "WordFlow source"
  # Use the checkout this script lives in, if it was run from one.
  # When piped from curl there's no script file, so this is skipped.
  local here=""
  if [ -f "${BASH_SOURCE[0]:-}" ]; then
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  fi
  if [ -n "$here" ] && [ -f "$here/scripts/setup.mjs" ]; then
    DIR="$here"
  elif [ ! -d "$DIR/.git" ]; then
    git clone "$REPO_URL" "$DIR" </dev/null
  fi
  echo "$DIR"

  cd "$DIR"
  npm run setup </dev/null
}

# Everything runs from main so `curl | bash` reads the whole script before starting.
main "$@"
