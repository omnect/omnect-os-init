# omnect-os-init

Rust-based init process for omnect-os initramfs.

## Overview

This project replaces the bash-based initramfs scripts with a type-safe Rust
implementation. It provides:

- **Bootloader abstraction**: Unified interface for GRUB and U-Boot environments
- **Partition management**: Device detection and `/dev/omnect/*` symlinks
- **Filesystem operations**: Mount, overlayfs, fsck
- **Factory reset**: Backup, wipe, restore operations
- **Flash modes**: Disk cloning and network flashing
- **ODS integration**: Runtime files for omnect-device-service

## Building

```bash
# Debug build
cargo build

# Release build (optimized for size)
cargo build --release

# With specific features
cargo build --release --features "flash-mode-2,resize-data"
```

## Features

| Feature | Description |
|---------|-------------|
| `core` | Core functionality (default) |
| `factory-reset` | Factory reset support |
| `flash-mode-1` | Disk cloning |
| `flash-mode-2` | Network flashing |
| `flash-mode-3` | HTTP/HTTPS flashing |
| `resize-data` | Data partition auto-resize |
| `persistent-var-log` | Persistent /var/log |

## Testing

```bash
# Run unit tests
cargo test

# Run with verbose output
cargo test -- --nocapture
```

## Architecture

```
src/
├── main.rs              # Entry point
├── lib.rs               # Library exports
├── error.rs             # Error types
├── bootloader/          # GRUB/U-Boot abstraction
│   ├── mod.rs           # Trait definition
│   ├── grub.rs          # GRUB implementation
│   ├── uboot.rs         # U-Boot implementation
│   └── types.rs         # Common types
├── config/              # Configuration loading
├── logging/             # Kernel message logging
└── ...                  # Additional modules (PR2+)
```

## License

MIT OR Apache-2.0
