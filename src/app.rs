// App state — composable streaming through the VMA → device → partition → fs chain.
//
// No temp files, no mmap, no caching.  Every operation creates a fresh
// VmaDeviceReader (File + cluster index) which reads directly from the
// uncompressed .vma file via seek+read.

use std::{
    fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use anyhow::{Context, Result};
use ratatui::widgets::ListState;

use crate::ext4::{DirEntry, Ext4Fs};
use crate::partition::{self, Partition, PartitionReader};
use crate::ui::ViewMode;
use crate::vma::{VmaArchive, VmaDeviceReader};

#[derive(Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: SystemTime,
    pub is_vma: bool,
}

pub struct App {
    // ── file list ──
    pub items: Vec<FileEntry>,
    pub list_state: ListState,
    pub current_path: PathBuf,

    // ── view state ──
    pub view_mode: ViewMode,
    pub quit: bool,
    pub show_help: bool,
    pub status_msg: String,

    // ── pending operation (for non-blocking UI updates) ──
    pub pending_op: Option<PendingOp>,

    // ── VMA ──
    pub vma: Option<VmaArchive>,
    pub selected_device: usize,
    pub config_scroll: usize,

    // ── partition table ──
    pub partitions: Vec<Partition>,
    pub partition_scroll: usize,

    // ── filesystem browser state ──
    ext4_partition_offset: u64,
    ext4_partition_length: u64,
    pub ext4_dirlist: Vec<DirEntry>,
    pub ext4_dirlist_state: ListState,
    pub ext4_current_dir: (u32, String),
    pub ext4_breadcrumbs: Vec<(u32, String)>,
    pub ext4_fs_type: String,
    pub ext4_total_blocks: u64,
    // Cached filesystem to avoid reopening on every navigation
    ext4_fs_cache: Option<Arc<Mutex<FsR>>>,

    // ── hex view ──
    pub hex_data: Vec<u8>,
    pub hex_scroll: usize,
    pub hex_title: String,
    pub dump_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum PendingOp {
    LoadVma(PathBuf),
    LoadPartitions,
    LoadDeviceRaw,
    LoadPartitionHex(usize),
    BrowsePartitionFs(usize),
}

// Thin wrapper so we have known concrete types
type DevR = VmaDeviceReader;
type PartR = PartitionReader<DevR>;
type FsR = Ext4Fs<PartR>;

impl App {
    pub fn new(path: PathBuf) -> Self {
        Self {
            items: vec![],
            list_state: ListState::default(),
            current_path: path,
            view_mode: ViewMode::FileList,
            quit: false,
            show_help: false,
            status_msg: String::new(),
            pending_op: None,
            vma: None,
            selected_device: 0,
            config_scroll: 0,
            partitions: vec![],
            partition_scroll: 0,
            ext4_partition_offset: 0,
            ext4_partition_length: 0,
            ext4_dirlist: vec![],
            ext4_dirlist_state: ListState::default(),
            ext4_current_dir: (2, "/".into()),
            ext4_breadcrumbs: vec![],
            ext4_fs_type: String::new(),
            ext4_total_blocks: 0,
            ext4_fs_cache: None,
            hex_data: vec![],
            hex_scroll: 0,
            hex_title: String::new(),
            dump_path: None,
        }
    }

    // ── Reader constructors ────────────────────────────────────────────

    /// Open a fresh VmaDeviceReader for the selected device.
    fn dev_reader(&self) -> Result<DevR> {
        let vma = self.vma.as_ref().context("no VMA")?;
        vma.open_device(self.selected_device)
    }

    /// Open a PartitionReader over the selected device, restricted to [byte_offset, byte_length).
    fn part_reader(&self, byte_offset: u64, byte_length: u64) -> Result<PartR> {
        let dr = self.dev_reader()?;
        Ok(PartitionReader::new(dr, byte_offset, byte_length))
    }

    /// Get a reference to the cached filesystem, opening it if needed.
    fn get_cached_fs(&mut self) -> Result<Arc<Mutex<FsR>>> {
        if self.ext4_fs_cache.is_none() {
            let pr = self.part_reader(self.ext4_partition_offset, self.ext4_partition_length)?;
            let fs = Ext4Fs::open(pr)?;
            self.ext4_fs_cache = Some(Arc::new(Mutex::new(fs)));
        }
        Ok(self.ext4_fs_cache.as_ref().unwrap().clone())
    }

    // ── File list ──────────────────────────────────────────────────────

    pub fn load_directory(&mut self, path: &Path) -> Result<()> {
        self.current_path = path.to_path_buf();
        self.items.clear();
        self.view_mode = ViewMode::FileList;
        self.vma = None;
        self.partitions.clear();
        self.selected_device = 0;
        self.config_scroll = 0;

        let Ok(entries) = fs::read_dir(path) else {
            return Ok(());
        };
        let mut dirs = vec![];
        let mut files = vec![];
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else {
                continue;
            };
            let is_dir = meta.is_dir();
            let name = e.file_name().to_string_lossy().to_string();
            let is_vma = name.ends_with(".vma")
                || name.ends_with(".vma.zst")
                || name.ends_with(".vma.xz");
            if !is_dir && !is_vma {
                continue;
            }
            let item = FileEntry {
                path: e.path(),
                is_dir,
                size: if is_dir { 0 } else { meta.len() },
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                is_vma,
            };
            if is_dir {
                dirs.push(item);
            } else {
                files.push(item);
            }
        }
        dirs.sort_by(|a, b| a.path.file_name().unwrap().cmp(b.path.file_name().unwrap()));
        files.sort_by(|a, b| a.path.file_name().unwrap().cmp(b.path.file_name().unwrap()));
        self.items = dirs.into_iter().chain(files).collect();
        if !self.items.is_empty() {
            self.list_state.select(Some(0));
        }
        Ok(())
    }

    // ── VMA loading ────────────────────────────────────────────────────

    pub fn load_vma(&mut self, path: &Path) -> Result<()> {
        self.status_msg = "Building VMA index...".into();
        let archive = VmaArchive::open(path)?;
        self.selected_device = 0;
        self.partitions.clear();
        self.vma = Some(archive);
        self.view_mode = ViewMode::VmaInfo;
        self.status_msg.clear();
        Ok(())
    }

    /// For the UI: whether we have a VMA loaded (device data is always
    /// available on-demand now).
    pub fn has_device_data(&self) -> bool {
        self.vma.is_some()
    }

    pub fn clear_device(&mut self) {
        // No-op — no temp files anymore
    }

    /// Execute any pending operation and clear it
    pub fn execute_pending(&mut self) -> Result<()> {
        if let Some(op) = self.pending_op.take() {
            match op {
                PendingOp::LoadVma(path) => self.load_vma(&path)?,
                PendingOp::LoadPartitions => self.load_partitions()?,
                PendingOp::LoadDeviceRaw => self.load_device_raw()?,
                PendingOp::LoadPartitionHex(pi) => self.load_partition_hex(pi)?,
                PendingOp::BrowsePartitionFs(pi) => self.browse_partition_fs(pi)?,
            }
        }
        Ok(())
    }

    // ── Partition table ────────────────────────────────────────────────

    pub fn load_partitions(&mut self) -> Result<()> {
        self.status_msg = "Reading partition table...".into();

        let vma = self.vma.as_ref().context("no VMA")?;
        let dev = &vma.devices[self.selected_device];

        // Read first 2 MB via VmaDeviceReader
        let mut dr = vma.open_device(self.selected_device)?;
        let read_len = (2 * 1024 * 1024).min(dev.size as usize);
        let mut data = vec![0u8; read_len];
        dr.seek(std::io::SeekFrom::Start(0))?;
        dr.read_exact(&mut data)?;

        self.dump_path = Some(save_dump(
            &data,
            &format!("{}_sectors_0_{}.bin", dev.name.replace('/', "_"), read_len / 512),
        )?);

        self.partitions = partition::parse_partition_table(&data);

        // Detect filesystem for each partition by probing their superblocks
        // For partitions beyond our 2MB buffer, we need to read directly from the device
        for p in &mut self.partitions {
            let probe_offset = p.byte_offset as usize;
            
            // Try to probe from the 2MB buffer first
            if probe_offset + 4 <= data.len() {
                let fs = partition::detect_fs_at(&data[probe_offset..]);
                if !fs.is_empty() {
                    p.fs_type = fs;
                    continue;
                }
            }
            
            // If partition is beyond 2MB, read a probe directly from the device
            if probe_offset >= data.len() {
                if let Ok(mut dr) = vma.open_device(self.selected_device) {
                    let probe_size = 2048usize.min(p.byte_length as usize);
                    if probe_size > 0 {
                        dr.seek(std::io::SeekFrom::Start(p.byte_offset)).ok();
                        let mut probe_buf = vec![0u8; probe_size];
                        if dr.read_exact(&mut probe_buf).is_ok() {
                            let fs = partition::detect_fs_at(&probe_buf);
                            if !fs.is_empty() {
                                p.fs_type = fs;
                            }
                        }
                    }
                }
            }
        }

        // Also add a "Raw device" entry with filesystem detection
        let mut found_fs = String::new();
        for off in &[0usize, 1024, 65536] {
            if *off + 4 <= data.len() {
                let fs = partition::detect_fs_at(&data[*off..]);
                if !fs.is_empty() {
                    found_fs = fs;
                    break;
                }
            }
        }
        if !found_fs.is_empty() {
            self.partitions.insert(
                0,
                Partition {
                    number: 0,
                    name: format!("Raw device ({})", dev.name),
                    start_sector: 0,
                    num_sectors: dev.size / 512,
                    type_guid: "00".into(),
                    fs_type: found_fs,
                    byte_offset: 0,
                    byte_length: dev.size,
                },
            );
        }

        self.view_mode = ViewMode::PartitionTable;
        self.partition_scroll = 0;
        self.status_msg.clear();
        Ok(())
    }

    // ── Hex dumps ─────────────────────────────────────────────────────

    pub fn load_partition_hex(&mut self, pi: usize) -> Result<()> {
        let p = self.partitions[pi].clone();
        self.status_msg = format!("Reading {}...", p.name);

        let mut dr = self.dev_reader()?;
        dr.seek(std::io::SeekFrom::Start(p.byte_offset))?;
        let len = 8192u64.min(p.byte_length);
        let mut buf = vec![0u8; len as usize];
        dr.read_exact(&mut buf)?;

        self.hex_data = buf;
        self.hex_scroll = 0;
        self.hex_title = format!("{} ({})", p.name, p.fs_type);
        self.view_mode = ViewMode::HexView;
        self.status_msg.clear();
        Ok(())
    }

    pub fn load_device_raw(&mut self) -> Result<()> {
        self.status_msg = "Reading raw data...".into();

        let mut dr = self.dev_reader()?;
        dr.seek(std::io::SeekFrom::Start(0))?;
        let dev = &self.vma.as_ref().unwrap().devices[self.selected_device];
        let len = 65536u64.min(dev.size);
        let mut buf = vec![0u8; len as usize];
        dr.read_exact(&mut buf)?;

        self.hex_data = buf;
        self.hex_scroll = 0;
        self.hex_title = format!("{} (raw, first 64KB)", dev.name);
        self.view_mode = ViewMode::HexView;
        self.status_msg.clear();
        Ok(())
    }

    // ── Filesystem browser ─────────────────────────────────────────────

    pub fn browse_partition_fs(&mut self, pi: usize) -> Result<()> {
        let p = self.partitions[pi].clone();
        self.status_msg = format!("Opening ext4 on {}...", p.name);

        self.ext4_partition_offset = p.byte_offset;
        self.ext4_partition_length = p.byte_length;
        self.ext4_fs_cache = None; // Reset cache for new partition

        let fs = self.get_cached_fs()?;
        let mut locked = fs.lock().unwrap();
        let fs_type = locked.fs_type.clone();
        let total_blocks = locked.total_blocks;
        let dirlist = locked.read_directory(2)?;
        drop(locked);

        self.ext4_fs_type = fs_type;
        self.ext4_total_blocks = total_blocks;
        self.ext4_dirlist = dirlist;
        self.ext4_current_dir = (2, "/".into());
        self.ext4_breadcrumbs = vec![(2, "/".into())];
        if !self.ext4_dirlist.is_empty() {
            self.ext4_dirlist_state.select(Some(0));
        }
        self.view_mode = ViewMode::FsBrowse;
        self.status_msg.clear();
        Ok(())
    }

    pub fn fs_navigate_into(&mut self, idx: usize) -> Result<()> {
        let entry = self.ext4_dirlist[idx].clone();
        if entry.ino == 0 {
            return Ok(());
        }

        if entry.name == ".." && self.ext4_breadcrumbs.len() > 1 {
            self.ext4_breadcrumbs.pop();
            let (pino, ppath) = self.ext4_breadcrumbs.last().cloned().unwrap();
            self.ext4_current_dir = (pino, ppath.clone());
            return self.fs_read_dir(pino);
        }
        if entry.name == "." {
            return Ok(());
        }

        // Use file_type field from DirEntry - 2 = directory, 1 = regular file
        // Don't call is_dir() as it triggers dir_get_entries which panics on files
        if entry.file_type != 2 {
            // Not a directory, skip navigation
            return Ok(());
        }

        let ino = entry.ino;
        let name = entry.name.clone();

        let fs = self.get_cached_fs()?;
        let mut locked = fs.lock().unwrap();
        let entries = locked.read_directory(ino)?;
        drop(locked);

        let path = format!("{}/{}", self.ext4_current_dir.1, name);
        self.ext4_breadcrumbs.push((ino, path.clone()));
        self.ext4_current_dir = (ino, path);
        self.ext4_dirlist = entries;
        if !self.ext4_dirlist.is_empty() {
            self.ext4_dirlist_state.select(Some(0));
        }
        Ok(())
    }

    pub fn fs_navigate_back(&mut self) -> Result<()> {
        if self.ext4_breadcrumbs.len() <= 1 {
            return Ok(());
        }
        self.ext4_breadcrumbs.pop();
        let (ino, path) = self.ext4_breadcrumbs.last().cloned().unwrap();
        self.ext4_current_dir = (ino, path);
        self.fs_read_dir(ino)
    }

    fn fs_read_dir(&mut self, ino: u32) -> Result<()> {
        let fs = self.get_cached_fs()?;
        let mut locked = fs.lock().unwrap();
        let entries = locked.read_directory(ino)?;
        drop(locked);
        self.ext4_dirlist = entries;
        if !self.ext4_dirlist.is_empty() {
            self.ext4_dirlist_state.select(Some(0));
        }
        Ok(())
    }

    /// Show hex view of a file in the filesystem browser
    pub fn fs_show_file_hex(&mut self, idx: usize) -> Result<()> {
        let entry = self.ext4_dirlist[idx].clone();
        if entry.file_type != 1 {
            return Ok(()); // Not a regular file
        }

        self.status_msg = format!("Reading {}...", entry.name);

        let fs = self.get_cached_fs()?;
        let mut locked = fs.lock().unwrap();
        let data = locked.read_file(entry.ino)?;
        drop(locked);

        self.hex_data = data;
        self.hex_scroll = 0;
        self.hex_title = format!("{} ({})", entry.name, self.ext4_current_dir.1);
        self.view_mode = ViewMode::HexView;
        self.status_msg.clear();
        Ok(())
    }

    /// Extract a file from the filesystem browser to disk
    pub fn fs_extract_file(&mut self, idx: usize, dest_path: &Path) -> Result<()> {
        let entry = self.ext4_dirlist[idx].clone();
        if entry.file_type != 1 {
            return Err(anyhow::anyhow!("Not a regular file"));
        }

        self.status_msg = format!("Extracting {}...", entry.name);

        let fs = self.get_cached_fs()?;
        let mut locked = fs.lock().unwrap();
        let data = locked.read_file(entry.ino)?;
        drop(locked);

        std::fs::write(dest_path, &data)?;
        
        self.status_msg = format!("Extracted {} ({} bytes)", entry.name, data.len());
        Ok(())
    }
}

pub fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{} B", size)
    } else if size < 1_048_576 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1_073_741_824 {
        format!("{:.1} MB", size as f64 / 1_048_576.0)
    } else {
        format!("{:.2} GB", size as f64 / 1_073_741_824.0)
    }
}

fn save_dump(data: &[u8], name: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(name);
    let mut f = std::fs::File::create(&path)?;
    f.write_all(data)?;
    Ok(path)
}
