# VZDump Browser

<div align="center">

**A TUI application for browsing Proxmox VZDump backups**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange)](https://www.rust-lang.org)
[![crates.io](https://img.shields.io/crates/v/vzdump-browser.svg)](https://crates.io/crates/vzdump-browser)

</div>

TUI application for browsing Proxmox VZDump backups (`.vma`, `.vma.zst`, `.vma.xz`).

Stream-decompresss huge VMA archives to disk, builds a cluster index for random-access reads, then lets you browse partition tables and hex dump individual sectors — all without loading the entire 200GB+ decompressed archive into RAM.

Stream-decompresses huge VMA archives to disk, builds a cluster index for random-access reads, then lets you browse partition tables and hex dump individual sectors — all without loading the entire 200GB+ decompressed archive into RAM.

## Features

- **File browser** — filter to only show directories and VMA backup files
- **VMA parser** — matches proxmox-vma header layout: devices, config files, timestamps, blob buffer
- **Stream decompression** — decompress to temp file on disk (handles 124GB compressed / 200GB+ decompressed)
- **Cluster index** — maps every extent cluster to a byte offset in the temp file for O(1) random access
- **Partition table** — MBR parsing, auto-detects filesystem type from superblock signatures
- **Filesystem detection** — ext2/3/4, XFS, btrfs, NTFS, LVM2
- **Hex viewer** — scrollable hex dump with ASCII sidebar

## Installation

### Build from Source

```bash
cargo build --release
```

### Requirements

- Rust 1.70+
- A TTY-compatible terminal (tmux, screen, or native terminal)

## Usage

```bash
./target/release/vzdump-browser -p ~/backups/
```

### Controls

#### File List

### File List
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
VMA file (.zst)
  │ zstd stream decompress ──► temp file on disk
  │ scan VMAE extent headers ──► cluster index (dev_id → [(cluster_num, file_offset)])
  │
  ▼
on-demand reads ──► read_device_sectors(dev, start, count)
                     │ binary search cluster index
                     │ seek + read from temp file
                     │
                     ▼ partition table + hex view
```

## Architecture

```
VMA file (.zst)
  │ zstd stream decompress ──► temp file on disk
  │ scan VMAE extent headers ──► cluster index (dev_id → [(cluster_num, file_offset)])
  │
  ▼
on-demand reads ──► read_device_sectors(dev, start, count)
                     │ binary search cluster index
                     │ seek + read from temp file
                     │
                     ▼ partition table + hex view
```

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.