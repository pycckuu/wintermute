#!/usr/bin/env bash
set -euo pipefail

REPO="pycckuu/wintermute"
INSTALL_DIR="${WINTERMUTE_INSTALL_DIR:-$HOME/.wintermute/bin}"

info()  { printf '\033[1;34m%s\033[0m\n' "$*"; }
warn()  { printf '\033[1;33m%s\033[0m\n' "$*"; }
error() { printf '\033[1;31m%s\033[0m\n' "$*" >&2; exit 1; }

detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *)      error "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              error "Unsupported architecture: $arch" ;;
    esac

    echo "${arch}-${os}"
}

latest_version() {
    local json version
    json="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")"
    if command -v jq &>/dev/null; then
        version="$(echo "$json" | jq -r '.tag_name' | sed 's/^v//')"
    else
        version="$(echo "$json" | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')"
    fi
    # Validate version format to prevent injection.
    if ! printf '%s' "$version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        error "Parsed version '${version}' does not match expected semver format"
    fi
    echo "$version"
}

main() {
    info "Wintermute installer"
    echo

    local target version archive url checksum_url
    target="$(detect_target)"
    info "Detected platform: ${target}"

    info "Fetching latest release..."
    version="$(latest_version)"
    if [ -z "$version" ]; then
        error "Could not determine latest version. Check https://github.com/${REPO}/releases"
    fi
    info "Latest version: v${version}"

    archive="wintermute-dist-${version}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/v${version}/${archive}"
    checksum_url="https://github.com/${REPO}/releases/download/v${version}/checksums-sha256.txt"

    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    info "Downloading ${archive}..."
    curl -fsSL -o "${tmpdir}/${archive}" "$url" \
        || error "Download failed. Check that a release exists for ${target}."

    info "Verifying checksum..."
    curl -fsSL -o "${tmpdir}/checksums-sha256.txt" "$checksum_url"
    expected="$(grep -F "${archive}" "${tmpdir}/checksums-sha256.txt" | awk '{print $1}')"
    if [ -n "$expected" ]; then
        if command -v sha256sum &>/dev/null; then
            actual="$(sha256sum "${tmpdir}/${archive}" | awk '{print $1}')"
        elif command -v shasum &>/dev/null; then
            actual="$(shasum -a 256 "${tmpdir}/${archive}" | awk '{print $1}')"
        else
            error "No sha256 tool found. Install coreutils or perl."
        fi
        if [ "$expected" != "$actual" ]; then
            error "Checksum mismatch! Expected ${expected}, got ${actual}"
        fi
        info "Checksum verified."
    else
        error "No checksum found for ${archive} in checksums-sha256.txt. Aborting."
    fi

    info "Installing to ${INSTALL_DIR}..."
    mkdir -p "$INSTALL_DIR"
    tar xzf "${tmpdir}/${archive}" -C "$tmpdir"
    cp "${tmpdir}/wintermute-${version}-${target}/wintermute" "$INSTALL_DIR/"
    cp "${tmpdir}/wintermute-${version}-${target}/flatline" "$INSTALL_DIR/"
    chmod +x "${INSTALL_DIR}/wintermute" "${INSTALL_DIR}/flatline"

    echo
    info "Installed successfully!"
    echo

    # Check PATH
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            warn "Add ${INSTALL_DIR} to your PATH:"
            echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo
            export PATH="${INSTALL_DIR}:$PATH"
            ;;
    esac

    # Check Docker
    if command -v docker &>/dev/null && docker info &>/dev/null; then
        info "Docker detected."
    else
        warn "Docker not found or not running."
        warn "Wintermute works best with Docker for sandboxed execution."
        warn "Install Docker: https://docs.docker.com/get-docker/"
        echo
    fi

    # Run init if not already configured
    if [ ! -d "$HOME/.wintermute" ]; then
        info "Running wintermute init..."
        "${INSTALL_DIR}/wintermute" init || true
    else
        info "~/.wintermute already exists, skipping init."
    fi

    echo
    info "Next steps:"
    echo "  1. Edit ~/.wintermute/.env with your API keys:"
    echo "     WINTERMUTE_TELEGRAM_TOKEN=your-bot-token"
    echo "     ANTHROPIC_API_KEY=your-api-key"
    echo
    echo "  2. Edit ~/.wintermute/config.toml:"
    echo "     Set allowed_users to your Telegram user ID"
    echo
    echo "  3. Start the agent:"
    echo "     wintermute start"
    echo
}

main "$@"
