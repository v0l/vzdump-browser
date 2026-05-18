// ext4 filesystem browser — using ext4_rs library.
//
// Ext4Fs wraps an existing Read+Seek (VmaDeviceReader or PartitionReader)
// and implements the BlockDevice trait for ext4_rs using interior mutability.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use ext4_rs::{BlockDevice, Ext4};

/// Wrapper around a Read+Seek that implements the BlockDevice trait.
/// Uses Arc<Mutex<>> for interior mutability since BlockDevice methods take &self.
pub struct BlockDeviceWrapper<R: Read + Seek> {
    inner: Mutex<R>,
    block_size: u32,
}

impl<R: Read + Seek> BlockDeviceWrapper<R> {
    pub fn new(inner: R, block_size: u32) -> Self {
        Self {
            inner: Mutex::new(inner),
            block_size,
        }
    }
}

impl<R: Read + Seek + Send + 'static> BlockDevice for BlockDeviceWrapper<R> {
    fn read_offset(&self, offset: usize) -> Vec<u8> {
        let mut guard = self.inner.lock().unwrap();
        let mut buf = vec![0u8; self.block_size as usize];
        let _ = guard.seek(SeekFrom::Start(offset as u64));
        let _ = guard.read_exact(&mut buf);
        buf
    }

    fn write_offset(&self, _offset: usize, _data: &[u8]) {
        // Read-only mode - not implemented
    }
}

/// Ext4 filesystem wrapper using ext4_rs.
pub struct Ext4Fs<R: Read + Seek + Send + 'static> {
    device: Arc<BlockDeviceWrapper<R>>,
    ext4: Ext4,
    pub block_size: u32,
    pub inodes_per_group: u32,
    pub fs_type: String,
    pub total_blocks: u64,
}

impl<R: Read + Seek + Send + 'static> Ext4Fs<R> {
    /// Open an ext4 filesystem from an existing Read+Seek handle.
    pub fn open(mut inner: R) -> Result<Self> {
        // First, read superblock to get block size
        inner.seek(SeekFrom::Start(1024))?;
        let mut sb_bytes = vec![0u8; 1024];
        inner.read_exact(&mut sb_bytes)?;

        // Check magic
        let magic = u16::from_le_bytes([sb_bytes[0x38], sb_bytes[0x39]]);
        if magic != 0xEF53 {
            bail!("not an ext2/3/4 filesystem (magic {:04X})", magic);
        }

        let log_block_size =
            u32::from_le_bytes([sb_bytes[0x18], sb_bytes[0x19], sb_bytes[0x1A], sb_bytes[0x1B]]);
        let block_size = 1024u32 << log_block_size;

        let inodes_per_group = u32::from_le_bytes([
            sb_bytes[0x28],
            sb_bytes[0x29],
            sb_bytes[0x2A],
            sb_bytes[0x2B],
        ]);

        // Read more superblock fields
        let blocks_lo = u32::from_le_bytes([sb_bytes[0x04], sb_bytes[0x05], sb_bytes[0x06], sb_bytes[0x07]]);
        let blocks_hi = u32::from_le_bytes([sb_bytes[0x150], sb_bytes[0x151], sb_bytes[0x152], sb_bytes[0x153]]);
        let total_blocks = if blocks_hi > 0 { (blocks_hi as u64) << 32 | blocks_lo as u64 } else { blocks_lo as u64 };

        // Feature flags to determine fs type
        let features = u32::from_le_bytes([sb_bytes[0x64], sb_bytes[0x65], sb_bytes[0x66], sb_bytes[0x67]]);
        let ro_features = u32::from_le_bytes([sb_bytes[0x68], sb_bytes[0x69], sb_bytes[0x6A], sb_bytes[0x6B]]);
        let is_ext4 = (features & 0x0040) != 0 || (ro_features & 0x0002) != 0 || (features & 0x0080) != 0;
        let fs_type = if is_ext4 { "ext4".into() } else { "ext2/3".into() };

        // Reset position
        inner.seek(SeekFrom::Start(0))?;

        // Create the block device wrapper
        let device = Arc::new(BlockDeviceWrapper::new(inner, block_size));

        // Open the ext4 filesystem (no Result)
        let ext4 = Ext4::open(device.clone());

        Ok(Ext4Fs {
            device,
            ext4,
            block_size,
            inodes_per_group,
            fs_type,
            total_blocks,
        })
    }

    /// Read a directory by inode number.
    pub fn read_directory(&mut self, ino: u32) -> Result<Vec<DirEntry>> {
        let entries = self.ext4.dir_get_entries(ino);
        let mut result = Vec::new();

        for entry in entries {
            // Extract name from the fixed-size array
            let name_len = entry.name_len as usize;
            let name = String::from_utf8_lossy(&entry.name[..name_len]).to_string();
            
            // Get file type from inode_type field (unsafe due to union)
            let file_type = unsafe {
                match entry.inner.inode_type {
                    2 => 2, // S_IFDIR
                    1 => 1, // S_IFREG  
                    7 => 7, // S_IFLNK
                    _ => 0,
                }
            };

            result.push(DirEntry {
                ino: entry.inode,
                name,
                file_type,
            });
        }

        Ok(result)
    }

    /// Check if an inode is a directory.
    pub fn is_dir(&self, ino: u32) -> bool {
        // Try to list directory entries - if it returns empty or fails, it's not a directory
        // Note: dir_get_entries may return empty for files, which is how we detect them
        let entries = self.ext4.dir_get_entries(ino);
        !entries.is_empty()
    }

    /// Get file size for an inode (regular files only).
    #[allow(dead_code)]
    pub fn file_size(&self, _ino: u32) -> Option<u64> {
        // ext4_rs doesn't expose inode size directly in simple interface
        // We'd need to use the fuse interface or get inode ref
        None
    }

    /// Read a file's contents by inode number.
    pub fn read_file(&mut self, ino: u32) -> Result<Vec<u8>> {
        // ext4_file_read returns up to 'size' bytes from the file at offset 0
        // We'll read up to 1MB - for larger files this would need chunking
        match self.ext4.ext4_file_read(ino as u64, 1024 * 1024, 0) {
            Ok(data) => Ok(data),
            Err(_) => Ok(Vec::new()), // Return empty on error
        }
    }

    /// Look up inode number by path.
    pub fn lookup_inode(&mut self, path: &str) -> Result<u32> {
        // Start from root and traverse the path
        let mut current_inode = 2u32; // ROOT_INODE
        let path = path.trim_start_matches('/');
        
        if path.is_empty() {
            return Ok(current_inode);
        }
        
        for component in path.split('/') {
            let entries = self.ext4.dir_get_entries(current_inode);
            let mut found = false;
            for entry in entries {
                let entry_name = String::from_utf8_lossy(&entry.name[..entry.name_len as usize]);
                if entry_name == component {
                    current_inode = entry.inode;
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(0); // Not found
            }
        }
        
        Ok(current_inode)
    }

    /// Read directory by path string.
    pub fn read_directory_str(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let ino = self.lookup_inode(path)?;
        if ino == 0 {
            return Err(anyhow::anyhow!("Path not found: {}", path));
        }
        self.read_directory(ino)
    }

    /// Get block size.
    #[allow(dead_code)]
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    #[allow(dead_code)]
    pub fn inodes_per_group(&self) -> u32 {
        self.inodes_per_group
    }

    #[allow(dead_code)]
    pub fn fs_type(&self) -> &str {
        &self.fs_type
    }

    #[allow(dead_code)]
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub ino: u32,
    pub name: String,
    pub file_type: u8,
}
