mod vma;
mod partition;
mod app;
mod ui;
mod keys;
mod ext4;
mod fatfs;

use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;

use crate::ext4::{DirEntry, Ext4Fs};
use crate::partition::{Partition, PartitionReader};
use crate::vma::VmaArchive;
use app::App;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to directory or VMA file (for interactive mode)
    #[arg(short, long)]
    path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Dump VMA contents: header, partitions, filesystem trees
    Dump {
        /// Path to VMA file
        vma: PathBuf,

        /// Maximum directory depth to descend (default: 4)
        #[arg(long, default_value = "4")]
        depth: u32,

        /// Maximum entries per directory (default: 200)
        #[arg(long, default_value = "200")]
        max_entries: usize,
    },

    /// List directory contents
    List {
        /// Path to VMA file
        #[arg(short, long)]
        vma: PathBuf,

        /// VMA device number (0, 1, etc.)
        #[arg(long, default_value = "0")]
        vma_device: usize,

        /// Partition number (0, 1, 2, etc.)
        #[arg(long, default_value = "0")]
        partition: usize,

        /// Path within filesystem
        #[arg(long, default_value = "/")]
        path: String,
    },

    /// Extract a file to disk
    Extract {
        /// Path to VMA file
        #[arg(short, long)]
        vma: PathBuf,

        /// VMA device number (0, 1, etc.)
        #[arg(long, default_value = "0")]
        vma_device: usize,

        /// Partition number (0, 1, 2, etc.)
        #[arg(long, default_value = "0")]
        partition: usize,

        /// Path to file within filesystem
        #[arg(long)]
        path: String,

        /// Output file path (default: same name in current dir)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Show hex dump of a file
    Hex {
        /// Path to VMA file
        #[arg(short, long)]
        vma: PathBuf,

        /// VMA device number (0, 1, etc.)
        #[arg(long, default_value = "0")]
        vma_device: usize,

        /// Partition number (0, 1, 2, etc.)
        #[arg(long, default_value = "0")]
        partition: usize,

        /// Path to file within filesystem
        #[arg(long)]
        path: String,

        /// Max bytes to show
        #[arg(long, default_value = "256")]
        max_bytes: usize,
    },

    /// Show VMA info
    Info {
        /// Path to VMA file
        #[arg(short, long)]
        vma: PathBuf,
    },

    /// Compute SHA256 hash of a file without extracting
    Hash {
        /// Path to VMA file
        #[arg(short, long)]
        vma: PathBuf,

        /// VMA device number (0, 1, etc.)
        #[arg(long, default_value = "0")]
        vma_device: usize,

        /// Partition number (0, 1, 2, etc.)
        #[arg(long, default_value = "0")]
        partition: usize,

        /// Path to file within filesystem
        #[arg(long)]
        path: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Dump {
            vma,
            depth,
            max_entries,
        }) => run_dump(&vma, depth, max_entries),
        Some(Command::List { vma, vma_device, partition, path }) => run_list(&vma, vma_device, partition, &path),
        Some(Command::Extract { vma, vma_device, partition, path, output }) => run_extract(&vma, vma_device, partition, &path, output.as_ref()),
        Some(Command::Hex { vma, vma_device, partition, path, max_bytes }) => run_hex(&vma, vma_device, partition, &path, max_bytes),
        Some(Command::Info { vma }) => run_info(&vma),
        Some(Command::Hash { vma, vma_device, partition, path }) => run_hash(&vma, vma_device, partition, &path),
        None => run_tui(cli.path),
    }
}

fn run_tui(path_opt: Option<PathBuf>) -> Result<()> {
    let path = path_opt.unwrap_or_else(|| PathBuf::from("."));

    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    let mut app = App::new(path.clone());
    app.load_directory(&path)?;

    let result = run_app(&mut term, &mut app);

    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;

    if let Err(e) = result {
        eprintln!("Error: {:?}", e);
    }
    Ok(())
}

fn run_app(term: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        term.draw(|f| ui::render(f, app))?;
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if k.code == KeyCode::Char('?') {
                app.show_help = !app.show_help;
                continue;
            }
            
            match app.view_mode {
                ui::ViewMode::FileList => keys::handle_filelist(k, app)?,
                ui::ViewMode::VmaInfo => keys::handle_vma(k, app)?,
                ui::ViewMode::PartitionTable => keys::handle_partitions(k, app)?,
                ui::ViewMode::HexView => keys::handle_hex(k, app)?,
                ui::ViewMode::FsBrowse => keys::handle_fsbrowse(k, app)?,
            }
            
            // If a pending operation was set, redraw to show the status message
            // before executing the blocking operation
            if app.pending_op.is_some() {
                term.draw(|f| ui::render(f, app))?;
                app.execute_pending()?;
            }
            
            if app.quit {
                return Ok(());
            }
        }
    }
}

// ── Dump command — full recursive tree ─────────────────────────────────

fn run_dump(vma_path: &std::path::Path, max_depth: u32, max_entries: usize) -> Result<()> {
    eprintln!("Building VMA index (scanning extents)...");
    let archive = VmaArchive::open(vma_path)?;

    println!("VMA: {}", vma_path.display());
    println!("Created: {}", archive.ctime);
    println!();

    // Config files
    if !archive.config.is_empty() {
        println!("Config files ({}):", archive.config.len());
        for (name, val) in &archive.config {
            let preview: String = val.chars().take(120).filter(|c| *c != '\n').collect();
            println!("  {:<35} {}", name, preview);
        }
        println!();
    }

    // Per-device drill-down
    for (di, device) in archive.devices.iter().enumerate() {
        let bar = "━".repeat(60);
        println!(
            "{} Device {}: {} ({}) {}",
            bar, di, device.name, app::format_size(device.size), bar
        );

        // Read first 2 MB via VmaDeviceReader for partition table
        eprintln!("  Reading partition table for {}...", device.name);
        let mut dr = archive.open_device(di).context("open device")?;
        let read_len = (2 * 1024 * 1024).min(device.size as usize);
        let mut part_buf = vec![0u8; read_len];
        dr.seek(SeekFrom::Start(0))?;
        dr.read_exact(&mut part_buf)?;
        drop(dr); // done — close the reader

        let partitions = partition::parse_partition_table(&part_buf);

        // FS probe at common offsets
        let mut found_fs = String::new();
        for off in &[0usize, 1024, 65536] {
            if *off + 4 <= part_buf.len() {
                let fs = partition::detect_fs_at(&part_buf[*off..]);
                if !fs.is_empty() { found_fs = fs; break; }
            }
        }

        let mut all_partitions = partitions.clone();
        if !found_fs.is_empty() {
            let covered = all_partitions.iter().any(|p| p.byte_offset == 0 && p.byte_length == device.size);
            if !covered {
                all_partitions.insert(0, Partition {
                    number: 0,
                    name: format!("Raw device ({})", device.name),
                    start_sector: 0,
                    num_sectors: device.size / 512,
                    type_guid: "00".into(),
                    fs_type: found_fs,
                    byte_offset: 0,
                    byte_length: device.size,
                });
            }
        }

        if !all_partitions.is_empty() {
            println!("  Partitions:");
            println!("    {:<22} {:>10} {:>10} {:>8}  {}", "Name", "Start LBA", "Size", "Type", "FS");
            println!("    {}", "─".repeat(70));
            for p in &all_partitions {
                println!("    {:<22} {:>10} {:>10} {:>8}  {}",
                    p.name, p.start_sector, app::format_size(p.num_sectors * 512), p.type_guid, p.fs_type);
            }
        } else {
            println!("  No partition table or filesystem detected.");
            continue;
        }

        // ── Filesystem tree dumps ──
        for p in &all_partitions {
            let dr2 = archive.open_device(di).context("re-open device")?;
            let pr = PartitionReader::new(dr2, p.byte_offset, p.byte_length);

            // Try ext4 first
            match Ext4Fs::open(pr) {
                Ok(mut fs) => {
                    println!();
                    println!("  📁 /  ({} on {}, {} {}, {} blocks)",
                        p.name, device.name, fs.fs_type,
                        app::format_size(p.num_sectors * 512), fs.total_blocks);

                    let mut total = 0usize;
                    if let Err(e) = dump_dir_tree(&mut fs, 2, "", max_depth, max_entries, &mut total) {
                        eprintln!("  ⚠ tree walk error: {}", e);
                    }
                    if total >= max_entries {
                        println!("  ... (reached {} entry limit)", max_entries);
                    }
                }
                Err(e) => {
                    // Try FAT if ext4 fails
                    let dr3 = archive.open_device(di).context("re-open device")?;
                    let pr2 = PartitionReader::new(dr3, p.byte_offset, p.byte_length);
                    
                    match fatfs::FatFs::open(pr2) {
                        Ok(mut fs) => {
                            println!();
                            println!("  📁 /  ({} on {}, {} FAT)",
                                p.name, device.name, fs.fs_type);

                            let mut total = 0usize;
                            if let Err(e) = dump_fat_dir_tree(&mut fs, "", max_depth, max_entries, &mut total) {
                                eprintln!("  ⚠ tree walk error: {}", e);
                            }
                            if total >= max_entries {
                                println!("  ... (reached {} entry limit)", max_entries);
                            }
                        }
                        Err(e) => {
                            println!();
                            println!("  ⚠ {} — could not open FAT: {}", p.name, e);
                        }
                        Err(_) => {
                            println!();
                            println!("  ⚠ {} — could not open ext4: {}", p.name, e);
                        }
                    }
                }
            }
        }
        println!();
    }

    Ok(())
}

/// Recursively walk an ext4 directory tree and print it.
fn dump_dir_tree<R: Read + Seek + Send + 'static>(
    fs: &mut Ext4Fs<R>,
    ino: u32,
    prefix: &str,
    max_depth: u32,
    max_entries: usize,
    total: &mut usize,
) -> Result<()> {
    use std::io::Write;

    if max_depth == 0 {
        return Ok(());
    }

    let entries = fs.read_directory(ino)?;

    // Sort: directories first, then alphabetical
    let mut items: Vec<&DirEntry> = entries
        .iter()
        .filter(|e| e.name != "." && e.name != "..")
        .collect();
    items.sort_by(|a, b| {
        let a_dir = a.file_type == 2;
        let b_dir = b.file_type == 2;
        b_dir.cmp(&a_dir).then(a.name.cmp(&b.name))
    });

    let count = items.len();
    for (i, entry) in items.iter().enumerate() {
        if *total >= max_entries {
            return Ok(());
        }
        *total += 1;

        let is_last = i == count.saturating_sub(1);
        let branch = if is_last { "└── " } else { "├── " };
        let icon = match entry.file_type {
            2 => "📁",
            1 => "📄",
            7 => "🔗",
            _ => "❓",
        };

        println!("{}{}{} {}", prefix, branch, icon, entry.name);

        if entry.file_type == 2 {
            let new_prefix = if is_last {
                format!("{}     ", prefix)
            } else {
                format!("{}│    ", prefix)
            };
            // Flush stdout so user sees progress
            let _ = std::io::stdout().flush();
            dump_dir_tree(fs, entry.ino, &new_prefix, max_depth - 1, max_entries, total)?;
        }
    }

    Ok(())
}

/// Recursively walk a FAT directory tree and print it.
fn dump_fat_dir_tree<R: Read + Write + Seek + Send + 'static>(
    fs: &mut fatfs::FatFs<R>,
    prefix: &str,
    max_depth: u32,
    max_entries: usize,
    total: &mut usize,
) -> Result<()> {
    use std::io::Write;

    if max_depth == 0 {
        return Ok(());
    }

    let entries = fs.read_directory(prefix)?;

    // Sort: directories first, then alphabetical
    let mut items: Vec<&fatfs::DirEntry> = entries
        .iter()
        .filter(|e| e.name != "." && e.name != "..")
        .collect();
    items.sort_by(|a, b| {
        let a_dir = a.file_type == 2;
        let b_dir = b.file_type == 2;
        b_dir.cmp(&a_dir).then(a.name.cmp(&b.name))
    });

    let count = items.len();
    for (i, entry) in items.iter().enumerate() {
        if *total >= max_entries {
            return Ok(());
        }
        *total += 1;

        let is_last = i == count.saturating_sub(1);
        let branch = if is_last { "└── " } else { "├── " };
        let icon = match entry.file_type {
            2 => "📁",
            1 => "📄",
            7 => "🔗",
            _ => "❓",
        };

        println!("{}{}{} {}", prefix, branch, icon, entry.name);

        if entry.file_type == 2 {
            let new_path = if prefix.is_empty() {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", prefix, entry.name)
            };
            let new_prefix = if is_last {
                format!("{}     ", prefix)
            } else {
                format!("{}│    ", prefix)
            };
            // Flush stdout so user sees progress
            let _ = std::io::stdout().flush();
            dump_fat_dir_tree(fs, &new_path, max_depth - 1, max_entries, total)?;
        }
    }

    Ok(())
}

// ── CLI commands for LLM tool use ─────────────────────────────────────

/// Helper to open VMA and get device reader
fn open_vma_device(vma_path: &PathBuf, device_idx: usize) -> Result<(VmaArchive, crate::vma::VmaDeviceReader)> {
    let archive = VmaArchive::open(vma_path)?;
    let reader = archive.open_device(device_idx)?;
    Ok((archive, reader))
}

/// Helper to get partition data
fn get_partition_data(archive: &VmaArchive, device_idx: usize) -> Result<Vec<Partition>> {
    let device = &archive.devices[device_idx];
    let mut dr = archive.open_device(device_idx)?;
    let read_len = (2 * 1024 * 1024).min(device.size as usize);
    let mut part_buf = vec![0u8; read_len];
    dr.seek(SeekFrom::Start(0))?;
    dr.read_exact(&mut part_buf)?;
    drop(dr);
    Ok(partition::parse_partition_table(&part_buf))
}

/// List directory contents
fn run_list(vma_path: &PathBuf, device_idx: usize, partition_idx: usize, path: &str) -> Result<()> {
    let (archive, mut dr) = open_vma_device(vma_path, device_idx)?;
    let partitions = get_partition_data(&archive, device_idx)?;
    
    if partition_idx >= partitions.len() {
        return Err(anyhow::anyhow!("Partition {} not found (only {} partitions)", partition_idx, partitions.len()));
    }
    
    let partition = &partitions[partition_idx];
    eprintln!("Opening {} on partition {}...", path, partition.name);
    
    // Create a partition reader
    let partition_reader = PartitionReader::new(dr, partition.byte_offset, partition.byte_length);
    
    // Try ext4 first
    if let Ok(mut fs) = Ext4Fs::open(partition_reader) {
        let entries = fs.read_directory_str(path)?;
        
        println!("{} ({})", path, partition.name);
        for entry in entries {
            let icon = match entry.file_type {
                2 => "📁",
                1 => "📄",
                7 => "🔗",
                _ => "❓",
            };
            println!("  {} {}", icon, entry.name);
        }
        return Ok(());
    }
    
    Err(anyhow::anyhow!("Unsupported filesystem or path not found"))
}

/// Extract a file to disk
fn run_extract(vma_path: &PathBuf, device_idx: usize, partition_idx: usize, path: &str, output: Option<&PathBuf>) -> Result<()> {
    let (archive, mut dr) = open_vma_device(vma_path, device_idx)?;
    let partitions = get_partition_data(&archive, device_idx)?;
    
    if partition_idx >= partitions.len() {
        return Err(anyhow::anyhow!("Partition {} not found", partition_idx));
    }
    
    let partition = &partitions[partition_idx];
    eprintln!("Extracting {}...", path);
    
    // Create a partition reader
    let partition_reader = PartitionReader::new(dr, partition.byte_offset, partition.byte_length);
    
    // Try ext4 first
    if let Ok(mut fs) = Ext4Fs::open(partition_reader) {
        let ino = fs.lookup_inode(path)?;
        if ino == 0 {
            return Err(anyhow::anyhow!("File not found: {}", path));
        }
        
        let data = fs.read_file(ino)?;
        let output_path = output.map(|p| p.clone()).unwrap_or_else(|| PathBuf::from(path.trim_start_matches('/')));
        std::fs::write(&output_path, &data)?;
        println!("Extracted {} ({} bytes) -> {}", path, data.len(), output_path.display());
        return Ok(());
    }
    
    Err(anyhow::anyhow!("Unsupported filesystem or path not found"))
}

/// Show hex dump of a file
fn run_hex(vma_path: &PathBuf, device_idx: usize, partition_idx: usize, path: &str, max_bytes: usize) -> Result<()> {
    let (archive, mut dr) = open_vma_device(vma_path, device_idx)?;
    let partitions = get_partition_data(&archive, device_idx)?;
    
    if partition_idx >= partitions.len() {
        return Err(anyhow::anyhow!("Partition {} not found", partition_idx));
    }
    
    let partition = &partitions[partition_idx];
    eprintln!("Reading {}...", path);
    
    // Create a partition reader
    let partition_reader = PartitionReader::new(dr, partition.byte_offset, partition.byte_length);
    
    // Try ext4 first
    if let Ok(mut fs) = Ext4Fs::open(partition_reader) {
        let ino = fs.lookup_inode(path)?;
        if ino == 0 {
            return Err(anyhow::anyhow!("File not found: {}", path));
        }
        
        let data = fs.read_file(ino)?;
        let data = &data[..max_bytes.min(data.len())];
        println!("Hex dump of {} ({} bytes):", path, data.len());
        for (i, chunk) in data.chunks(16).enumerate() {
            let offset = i * 16;
            let hex: String = chunk.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
            let ascii: String = chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }).collect();
            println!("{:08x}:  {:<48}  {}", offset, hex, ascii);
        }
        return Ok(());
    }
    
    Err(anyhow::anyhow!("Unsupported filesystem or path not found"))
}

/// Show VMA info
fn run_info(vma_path: &PathBuf) -> Result<()> {
    eprintln!("Building VMA index...");
    let archive = VmaArchive::open(vma_path)?;
    
    println!("VMA: {}", vma_path.display());
    println!("Created: {}", archive.ctime);
    println!("Devices: {}", archive.devices.len());
    for (i, dev) in archive.devices.iter().enumerate() {
        println!("  {}: {} ({})", i, dev.name, app::format_size(dev.size));
    }
    Ok(())
}

/// Compute SHA256 hash of a file without extracting
fn run_hash(vma_path: &PathBuf, device_idx: usize, partition_idx: usize, path: &str) -> Result<()> {
    let (archive, mut dr) = open_vma_device(vma_path, device_idx)?;
    let partitions = get_partition_data(&archive, device_idx)?;
    
    if partition_idx >= partitions.len() {
        return Err(anyhow::anyhow!("Partition {} not found", partition_idx));
    }
    
    let partition = &partitions[partition_idx];
    eprintln!("Computing hash for {}...", path);
    
    // Create a partition reader
    let partition_reader = PartitionReader::new(dr, partition.byte_offset, partition.byte_length);
    
    // Try ext4 first
    if let Ok(mut fs) = Ext4Fs::open(partition_reader) {
        let ino = fs.lookup_inode(path)?;
        if ino == 0 {
            return Err(anyhow::anyhow!("File not found: {}", path));
        }
        
        let data = fs.read_file(ino)?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash_hex = hex::encode(hasher.finalize());
        
        println!("File: {}", path);
        println!("Size: {} bytes", data.len());
        println!("SHA256: {}", hash_hex);
        return Ok(());
    }
    
    Err(anyhow::anyhow!("Unsupported filesystem or path not found"))
}
