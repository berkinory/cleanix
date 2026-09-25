# cleanix

A developer cleanup tool for macOS and Linux, built with Rust and Ratatui.

## Install

```sh
curl -fsSL https://github.com/berkinory/cleanix/releases/latest/download/install.sh | sh
```

The installer downloads the binary for your platform, checks its SHA-256 checksum, and installs it to `~/.local/bin`. No Rust toolchain or sudo required.

Supports Apple Silicon and Intel Macs on macOS 11+, and ARM64/x86_64 Linux with glibc 2.35+.

## Use

```sh
cleanix          # interactive cleanup
cleanix --dry    # validate selections without deleting
cleanix --scan   # read-only inventory
cleanix --json   # read-only inventory as JSON
```

`↑/↓` navigate · `enter` expand · `space` select · `d` delete · `r` rescan  
`/` search · `i` scan details · `?` help · `q` quit

**Deletion is permanent and requires confirmation. Files are not moved to Trash.**

## What it finds

- Project dependencies and generated output: `node_modules`, Python environments, Unity, Gradle, SwiftPM, and web build caches.
- Package and application caches: npm, npx, Bun, pnpm, Cargo, Go, pip, uv, and more.
- Stopped Android devices and snapshots, downloaded models, and unused local Docker resources.
- On macOS: Xcode data and archives, iOS simulators, old temporary files, Trash, and Time Machine snapshots.

Nothing is selected automatically. Installed Android SDK components and language toolchains are excluded. Some items contain data worth keeping, such as archives, device data, or downloaded models; their descriptions explain what removal means.

On macOS, grant your terminal **Full Disk Access** to read privacy-restricted locations such as Trash, then restart the terminal and rescan. Unreadable paths are excluded from the list and total; restricted access triggers a warning at the top.

Linux administrator actions use polkit and require an authentication agent.

## Scope

Project discovery starts at your home directory. Set a different root, or pass several:

```sh
cleanix --root ~/work --root ~/personal
```

Known tool and app caches are scanned separately. Optional settings live in `~/.config/cleanix/config.toml` or `$XDG_CONFIG_HOME/cleanix/config.toml`; no config file is needed to get started.

## Contribute

With Rust installed, run `./cleanix` to build and launch locally. Run `make test` before opening a PR.

[MIT licensed](LICENSE).
