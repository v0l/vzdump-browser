# VZDump Browser

<div align="center">

**A TUI application for browsing Proxmox VZDump backups**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange)](https://www.rust-lang.org)

</div>

TUI application for browsing Proxmox VZDump backups (`.vma` files only - **uncompressed**).

Stream-reads VMA archives with a cluster index for random-access reads, letting you browse partition tables, filesystems (ext4, FAT), and hex dump individual sectors — all without loading the entire archive into RAM.

**Note:** Compressed backups (`.vma.zst`, `.vma.xz`) must be decompressed first before using this tool.

## Features

- **File browser** — filter to only show directories and VMA backup files
- **VMA parser** — matches proxmox-vma header layout: devices, config files, timestamps, blob buffer
- **Cluster index** — maps every extent cluster to a byte offset for O(1) random access
- **Partition table** — MBR/GPT parsing, auto-detects filesystem type from superblock signatures
- **Filesystem detection** — ext2/3/4, XFS, btrfs, NTFS, LVM2, FAT
- **Hex viewer** — scrollable hex dump with ASCII sidebar
- **File extraction** — extract files from ext4 filesystems to disk

## Installation

### Build from Source

```bash
cargo build --release
```

### Requirements

- Rust 1.70+
- A TTY-compatible terminal (tmux, screen, or native terminal)
- **Uncompressed `.vma` files only** (decompress `.vma.zst` with `zstd -d` first)

## Usage

```bash
./target/release/vzdump-browser -p ~/backups/
```

### Controls

#### File List

| Key | Action |
|-----|--------|
| `j`/`k` or `↑`/`↓` | Navigate |
| `Enter`/`l`/`→` | Open file/directory |
| `h`/`←`/`Backspace` | Go up |
| `q` | Quit |
| `?` | Help |

#### VMA Info

| Key | Action |
|-----|--------|
| `w`/`s` | Select device |
| `Enter`/`p` | Analyze partitions |
| `x` | Raw hex view |
| `j`/`k` | Scroll config files |
| `t` | Back |

#### Partitions / Hex

| Key | Action |
|-----|--------|
| `j`/`k` | Scroll/select |
| `Enter` | View hex dump |
| `PgUp`/`PgDn` | Page scroll |
| `h`/`t` | Back |

## Architecture

```
Uncompressed VMA file (.vma)
  │ parse header → build cluster index (dev_id → [(cluster_num, file_offset)])
  │
  ▼
on-demand reads ──► VmaDeviceReader (File + cluster index lookup)
                     │ binary search cluster index
                     │ seek + read from .vma file directly
                     │
                     ▼ partition table + filesystem parsing + hex view
```

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
