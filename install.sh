#!/bin/sh
# Build the Drift release bundles and install them into the CLAP and VST3 folders.
#
#   ./install.sh               build, then copy to ~/.clap and ~/.vst3
#   ./install.sh --system      install to /usr/lib/clap and /usr/lib/vst3 (uses sudo)
#   ./install.sh --link        symlink instead of copy, so later rebuilds need no reinstall
#   ./install.sh --no-build    install the bundles already in target/bundled
#   ./install.sh --uninstall   remove installed bundles from the chosen location
#   ./install.sh --clap | --vst3   only one format
#
# The build targets x86-64-v3 (AVX2, Haswell and later) by default. For older CPUs
# set TARGET_CPU, e.g. TARGET_CPU=x86-64 ./install.sh, or TARGET_CPU=native for
# the build machine only.
set -eu
cd "$(dirname "$0")"

NAME=Drift
BUNDLED=target/bundled
TARGET_CPU=${TARGET_CPU:-x86-64-v3}

clap=1
vst3=1
system=0
link=0
build=1
uninstall=0

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

for arg in "$@"; do
    case $arg in
        --system) system=1 ;;
        --link) link=1 ;;
        --no-build) build=0 ;;
        --uninstall) uninstall=1 ;;
        --clap) vst3=0 ;;
        --vst3) clap=0 ;;
        -h | --help) usage ;;
        *) echo "unknown option: $arg" >&2; usage 1 >&2 ;;
    esac
done

if [ "$clap" = 0 ] && [ "$vst3" = 0 ]; then
    echo "--clap and --vst3 exclude each other" >&2
    exit 1
fi

if [ "$system" = 1 ]; then
    CLAP_DIR=/usr/lib/clap
    VST3_DIR=/usr/lib/vst3
    SUDO=sudo
    [ "$(id -u)" = 0 ] && SUDO=
    if [ "$link" = 1 ]; then
        echo "--link with --system would point system folders into this checkout; refusing" >&2
        exit 1
    fi
else
    CLAP_DIR=$HOME/.clap
    VST3_DIR=$HOME/.vst3
    SUDO=
fi

# Remove first, then copy: overwriting a .so in place can crash a host that has
# it loaded, while a new inode leaves the running instance untouched.
install_bundle() {
    src=$BUNDLED/$1
    dest_dir=$2
    dest=$dest_dir/$1
    if [ ! -e "$src" ]; then
        echo "missing $src; run without --no-build" >&2
        exit 1
    fi
    $SUDO mkdir -p "$dest_dir"
    $SUDO rm -rf "$dest"
    if [ "$link" = 1 ]; then
        ln -s "$PWD/$src" "$dest"
    else
        $SUDO cp -R "$src" "$dest"
    fi
    echo "  $dest"
}

remove_bundle() {
    dest=$2/$1
    if [ -e "$dest" ] || [ -L "$dest" ]; then
        $SUDO rm -rf "$dest"
        echo "  removed $dest"
    fi
}

if [ "$uninstall" = 1 ]; then
    [ "$clap" = 1 ] && remove_bundle "$NAME.clap" "$CLAP_DIR"
    [ "$vst3" = 1 ] && remove_bundle "$NAME.vst3" "$VST3_DIR"
    exit 0
fi

if [ "$build" = 1 ]; then
    command -v cargo >/dev/null || { echo "cargo not found; install Rust from https://rustup.rs" >&2; exit 1; }
    echo "Building $NAME (target-cpu=$TARGET_CPU)"
    RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=$TARGET_CPU" cargo xtask bundle synth --release
fi

echo "Installing:"
[ "$clap" = 1 ] && install_bundle "$NAME.clap" "$CLAP_DIR"
[ "$vst3" = 1 ] && install_bundle "$NAME.vst3" "$VST3_DIR"
echo "Rescan plugins in the host. Use the CLAP build in Bitwig (VST3 editors render black there)."
