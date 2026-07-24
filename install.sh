#!/bin/sh
set -eu

main() {
    os=$(uname -s)
    if [ "$os" != "Linux" ]; then
        echo "Pan requires Linux. Your OS: $os" >&2
        echo "On macOS, build from source: https://github.com/chezgoulet/pan" >&2
        exit 1
    fi

    arch=$(uname -m)
    case "$arch" in
        x86_64|amd64) ;;
        *)
            echo "Unsupported architecture: $arch. Pan currently ships x86_64 binaries." >&2
            echo "Build from source: https://github.com/chezgoulet/pan" >&2
            exit 1
            ;;
    esac

    install_dir="${PREFIX:-$HOME/.local}/bin"
    mkdir -p "$install_dir"

    dl_url="https://github.com/chezgoulet/pan/releases/latest/download/pan"
    target="${install_dir}/pan"

    echo "Downloading pan..."
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$dl_url" -o "$target"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$dl_url" -O "$target"
    else
        echo "Need curl or wget to download. Install one and retry." >&2
        exit 1
    fi

    chmod +x "$target"

    echo "Installed pan to $target"

    if echo "$PATH" | tr ':' '\n' | grep -qxF "$install_dir"; then
        echo "Ready. Run 'pan --help' to get started."
    else
        echo ""
        echo "Note: $install_dir is not on your PATH."
        shell_name=$(basename "${SHELL:-sh}")
        echo "Add to your ~/.${shell_name}rc:"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        echo ""
        echo "Then open a new terminal or run 'source ~/.${shell_name}rc'."
    fi

    echo ""
    echo "Next steps:"
    echo "  pan --version"
    echo "  pan --help"
    echo "  pan run <Agent.toml>"
}

main
