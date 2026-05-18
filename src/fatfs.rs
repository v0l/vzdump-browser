// FAT filesystem browser — using rust-fatfs library.
//
// FatFs wraps an existing Read+Seek (VmaDeviceReader or PartitionReader)
// and provides directory listing capabilities for FAT filesystems.

use std::io::{Read, Seek, SeekFrom, Write};

use anyhow::{bail, Result};
use fatfs::{FileSystem, FsOptions};

/// FAT filesystem wrapper using rust-fatfs.
pub struct FatFs<R: Read + Write + Seek + Send + 'static> {
    fs: FileSystem<R>,
    pub fs_type: String, // FAT12, FAT16, or FAT32
}

impl<R: Read + Write + Seek + Send + 'static> FatFs<R> {
    /// Open a FAT filesystem from an existing Read+Seek handle.
    pub fn open(mut inner: R) -> Result<Self> {
        // Check for FAT signature at offset 0x1FE (510)
        inner.seek(SeekFrom::Start(510))?;
        let mut boot_sig = vec![0u8; 2];
        inner.read_exact(&mut boot_sig)?;
        
        if boot_sig != [0x55, 0xAA] {
            bail!("not a FAT filesystem (invalid boot signature)");
        }

        // Read BPB (BIOS Parameter Block) at offset 0x0B
        inner.seek(SeekFrom::Start(0x0B))?;
        let mut bpb = vec![0u8; 90];
        inner.read_exact(&mut bpb)?;

        let total_sectors_16 = u16::from_le_bytes([bpb[13], bpb[14]]);
        let total_sectors_32 = u32::from_le_bytes([bpb[19], bpb[20], bpb[21], bpb[22]]);
        let sectors_per_fat_32 = u32::from_le_bytes([bpb[24], bpb[25], bpb[26], bpb[27]]);
        let sectors_per_fat = u16::from_le_bytes([bpb[7], bpb[8]]);
        
        let total_sectors = if total_sectors_16 != 0 {
            total_sectors_16 as u32
        } else {
            total_sectors_32
        };

        let sectors_per_fat = if sectors_per_fat != 0 {
            sectors_per_fat as u32
        } else {
            sectors_per_fat_32
        };

        // Determine FAT type based on sector count
        let fat_type = if total_sectors < 4084 {
            "FAT12"
        } else if total_sectors < 65536 && sectors_per_fat == 0 {
            "FAT16"
        } else {
            "FAT32"
        };

        // Reset position to start
        inner.seek(SeekFrom::Start(0))?;

        // Open the filesystem
        let options = FsOptions::new();
        let fs = FileSystem::new(inner, options)
            .map_err(|e| anyhow::anyhow!("Failed to open FAT filesystem: {:?}", e))?;

        Ok(FatFs {
            fs,
            fs_type: fat_type.to_string(),
        })
    }

    /// Read a directory by path.
    pub fn read_directory(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let root_dir = self.fs.root_dir();
        let dir = if path.is_empty() || path == "/" {
            root_dir
        } else {
            root_dir.open_dir(path)?
        };
        
        let mut entries = Vec::new();

        for entry_result in dir.iter() {
            let entry = entry_result?;
            let name = entry.file_name();
            let file_type = if entry.is_dir() {
                2
            } else {
                1
            };

            entries.push(DirEntry {
                ino: 0, // FAT doesn't have inodes
                name,
                file_type,
            });
        }

        Ok(entries)
    }

    /// Check if a path is a directory.
    pub fn is_dir(&self, path: &str) -> bool {
        let root_dir = self.fs.root_dir();
        if path.is_empty() || path == "/" {
            return true;
        }
        root_dir.open_dir(path).is_ok()
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub ino: u32,
    pub name: String,
    pub file_type: u8,
}
