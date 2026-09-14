#!/usr/bin/env bash
set -Eeuo pipefail

MIN_RUST_MAJOR=1
MIN_RUST_MINOR=83
CHECK_ONLY=0
SKIP_BUILD=0

usage() {
    cat <<'EOF'
Usage: scripts/install-dependencies.sh [options]

Options:
  --check-only  Report missing dependencies. Do not install or build.
  --skip-build  Install dependencies. Do not run cargo check.
  -h, --help    Show this help.
EOF
}

log() {
    printf '[lpudp] %s\n' "$1"
}

fail() {
    printf '[lpudp] ERROR: %s\n' "$1" >&2
    exit 1
}

for argument in "$@"; do
    case "$argument" in
        --check-only) CHECK_ONLY=1 ;;
        --skip-build) SKIP_BUILD=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; fail "Unknown option: $argument" ;;
    esac
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
project_dir=$(cd -- "$script_dir/.." && pwd)
cd "$project_dir"

[[ -f Cargo.toml ]] || fail "Cargo.toml is not present in $project_dir."

if ! command -v apt-get >/dev/null 2>&1; then
    fail "This script supports Mint and other Debian-based systems with apt-get."
fi

if [[ "$CHECK_ONLY" -eq 0 ]]; then
    if [[ "$(id -u)" -eq 0 ]]; then
        sudo_command=()
    elif command -v sudo >/dev/null 2>&1; then
        sudo_command=(sudo)
        sudo -v
    else
        fail "Install sudo or run this script as root."
    fi
else
    sudo_command=()
fi

apt_packages=(
    ca-certificates
    curl
    git
    build-essential
    pkg-config
    pulseaudio-utils
)
missing_packages=()
for package in "${apt_packages[@]}"; do
    if ! dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q 'install ok installed'; then
        missing_packages+=("$package")
    fi
done

if [[ "${#missing_packages[@]}" -gt 0 ]]; then
    if [[ "$CHECK_ONLY" -eq 1 ]]; then
        log "Missing APT packages: ${missing_packages[*]}"
    else
        log "Installing APT packages: ${missing_packages[*]}"
        "${sudo_command[@]}" apt-get update
        "${sudo_command[@]}" apt-get install -y --no-install-recommends "${missing_packages[@]}"
    fi
else
    log "APT packages are present."
fi

if ! command -v paplay >/dev/null 2>&1; then
    log "WARNING: paplay is not available. Alert sound playback will not work."
fi

rust_version() {
    if ! command -v rustc >/dev/null 2>&1; then
        return 1
    fi
    rustc --version | awk '{print $2}'
}

rust_is_current() {
    local version major minor
    version=$(rust_version) || return 1
    major=${version%%.*}
    minor=${version#*.}
    minor=${minor%%.*}
    [[ "$major" -gt "$MIN_RUST_MAJOR" || ( "$major" -eq "$MIN_RUST_MAJOR" && "$minor" -ge "$MIN_RUST_MINOR" ) ]]
}

rust_toolchain_is_ready() {
    rust_is_current && command -v cargo >/dev/null 2>&1
}

if rust_toolchain_is_ready; then
    log "Rust $(rust_version) is present."
elif [[ "$CHECK_ONLY" -eq 1 ]]; then
    if command -v rustc >/dev/null 2>&1; then
        if ! rust_is_current; then
            log "Rust $(rust_version) is too old. Rust ${MIN_RUST_MAJOR}.${MIN_RUST_MINOR} or newer is required."
        else
            log "Cargo is not present. Install the Rust toolchain."
        fi
    else
        log "Rust is not present. Rust ${MIN_RUST_MAJOR}.${MIN_RUST_MINOR} or newer is required."
    fi
else
    cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    export PATH="$cargo_bin:$PATH"
    if ! command -v rustup >/dev/null 2>&1; then
        log "Installing rustup with the official Rust installer."
        rustup_temp_dir=$(mktemp -d)
        trap 'rm -rf "$rustup_temp_dir"' EXIT
        curl --proto '=https' --tlsv1.2 --fail --silent --show-error https://sh.rustup.rs -o "$rustup_temp_dir/rustup-init"
        chmod 700 "$rustup_temp_dir/rustup-init"
        "$rustup_temp_dir/rustup-init" -y --profile minimal --default-toolchain stable --no-modify-path
        export PATH="$cargo_bin:$PATH"
    fi
    rustup toolchain install stable --profile minimal --no-self-update
    rustup default stable
    rust_toolchain_is_ready || fail "The installed Rust toolchain is not ready."
    log "Rust $(rust_version) is ready."
fi

if [[ "$CHECK_ONLY" -eq 1 ]]; then
    if ! rust_toolchain_is_ready || [[ "${#missing_packages[@]}" -gt 0 ]]; then
        exit 1
    fi
    log "Dependency check passed."
    exit 0
fi

if [[ "$SKIP_BUILD" -eq 0 ]]; then
    log "Fetching locked Rust dependencies."
    cargo fetch --locked
    log "Compiling the project."
    cargo check --locked
fi

log "Dependency setup completed."
