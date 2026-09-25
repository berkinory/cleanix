#!/bin/sh
set -eu

main() {
    if [ "${1:-}" = --help ]; then
        printf '%s\n' 'Usage: sh install.sh [VERSION]' 'Installs cleanix into ${CLEANIX_INSTALL_DIR:-$HOME/.local/bin} after SHA-256 verification.'
        return
    fi
    [ "$#" -le 1 ] || { printf '%s\n' 'Expected at most one version argument.' >&2; return 1; }
    command -v curl >/dev/null || { printf '%s\n' 'curl is required.' >&2; return 1; }
    case "$(uname -s)" in
        Darwin) os=apple-darwin ;;
        Linux) os=unknown-linux-gnu ;;
        *) printf '%s\n' 'Only macOS and Linux are supported.' >&2; return 1 ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) arch=aarch64 ;;
        x86_64|amd64) arch=x86_64 ;;
        *) printf '%s\n' 'Only ARM64 and x86_64 are supported.' >&2; return 1 ;;
    esac
    repo=https://github.com/berkinory/cleanix
    version=${1:-}
    if [ -z "$version" ]; then
        url=$(curl --proto '=https' --tlsv1.2 -fsSL -o /dev/null -w '%{url_effective}' "$repo/releases/latest")
        version=${url##*/}
    fi
    case "$version" in v*) ;; *) version=v$version ;; esac
    case "$version" in *[!a-zA-Z0-9._-]*|v) printf '%s\n' 'Invalid version.' >&2; return 1 ;; esac
    case "$version" in v[0-9]*.[0-9]*.[0-9]*) ;; *) printf '%s\n' 'Expected a version such as 0.1.0.' >&2; return 1 ;; esac
    archive=cleanix-$arch-$os.tar.gz
    base=$repo/releases/download/$version
    tmp=$(mktemp -d)
    stage=
    trap 'rm -rf "$tmp"; if [ -n "$stage" ]; then rm -f "$stage"; fi' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    curl --proto '=https' --tlsv1.2 -fsSL "$base/$archive" -o "$tmp/$archive"
    curl --proto '=https' --tlsv1.2 -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
    expected=$(awk -v name="$archive" '$2 == name {print $1}' "$tmp/SHA256SUMS")
    [ "${#expected}" -eq 64 ] || { printf '%s\n' 'Missing or invalid SHA-256 checksum.' >&2; return 1; }
    case "$expected" in *[!0-9a-f]*) printf '%s\n' 'Invalid SHA-256 checksum.' >&2; return 1 ;; esac
    if command -v sha256sum >/dev/null; then
        actual=$(sha256sum "$tmp/$archive" | awk '{print $1}')
    elif command -v shasum >/dev/null; then
        actual=$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')
    else
        printf '%s\n' 'sha256sum or shasum is required.' >&2
        return 1
    fi
    [ "$actual" = "$expected" ] || { printf '%s\n' 'SHA-256 mismatch; nothing installed.' >&2; return 1; }
    tar -xzf "$tmp/$archive" -C "$tmp" cleanix
    [ -f "$tmp/cleanix" ] && [ ! -L "$tmp/cleanix" ] || { printf '%s\n' 'Invalid release binary.' >&2; return 1; }
    chmod 755 "$tmp/cleanix"
    actual_version=$("$tmp/cleanix" --version)
    [ "$actual_version" = "cleanix ${version#v}" ] || { printf '%s\n' 'Release version mismatch; nothing installed.' >&2; return 1; }
    dest=${CLEANIX_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}
    mkdir -p "$dest"
    [ ! -d "$dest/cleanix" ] || { printf '%s\n' 'Destination cleanix is a directory.' >&2; return 1; }
    stage=$(mktemp "$dest/.cleanix.XXXXXX")
    cp "$tmp/cleanix" "$stage"
    chmod 755 "$stage"
    mv -f "$stage" "$dest/cleanix"
    stage=
    printf 'Installed cleanix %s to %s/cleanix\n' "${version#v}" "$dest"
    case ":${PATH:-}:" in *":$dest:"*) ;; *) printf 'Add %s to PATH to run cleanix by name.\n' "$dest" ;; esac
}
main "$@"
