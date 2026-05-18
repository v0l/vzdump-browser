// Partition table parsing + composable PartitionReader<R: Read+Seek>.
//
// PartitionReader wraps an underlying Read+Seek (VmaDeviceReader, etc.)
// and restricts reads to a single partition's byte range.

use std::io::{self, Read, Seek, SeekFrom, Write};

#[derive(Clone, Debug)]
pub struct Partition {
    pub number: u8,
    pub name: String,
    pub start_sector: u64,
    pub num_sectors: u64,
    pub type_guid: String,
    pub fs_type: String,
    /// Byte offset of this partition within the parent device
    pub byte_offset: u64,
    /// Byte length of this partition
    pub byte_length: u64,
}

/// Restrict an underlying Read+Seek to a byte range.
/// Seeks are relative to the partition start; reads are clamped.
pub struct PartitionReader<R: Read + Seek> {
    inner: R,
    offset: u64,
    length: u64,
    pos: u64,
}

impl<R: Read + Seek> PartitionReader<R> {
    /// Create a new PartitionReader from an existing Read+Seek handle.
    /// `offset` and `length` are byte offsets within `inner`.
    pub fn new(inner: R, byte_offset: u64, byte_length: u64) -> Self {
        PartitionReader {
            inner,
            offset: byte_offset,
            length: byte_length,
            pos: 0,
        }
    }

    /// Get the underlying reader back (consumes PartitionReader).
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Borrow the inner reader.
    pub fn inner_ref(&self) -> &R {
        &self.inner
    }

    /// Mutably borrow the inner reader.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }
}

impl<R: Read + Seek> Read for PartitionReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.length {
            return Ok(0);
        }
        self.inner.seek(SeekFrom::Start(self.offset + self.pos))?;
        let max_read = (self.length - self.pos).min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..max_read])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for PartitionReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(off) => off as i64,
            SeekFrom::End(off) => self.length as i64 + off,
            SeekFrom::Current(off) => self.pos as i64 + off,
        };
        if new < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative seek",
            ));
        }
        self.pos = (new as u64).min(self.length);
        Ok(self.pos)
    }
}

impl<R: Read + Seek> Write for PartitionReader<R> {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        // Read-only - reject all writes
        Err(io::Error::new(io::ErrorKind::Other, "read-only"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ── Partition table parsing ────────────────────────────────────────────

pub fn parse_partition_table(data: &[u8]) -> Vec<Partition> {
    if data.len() < 512 || &data[510..512] != b"\x55\xAA" {
        return vec![];
    }

    // Check for GPT protective MBR (type 0xEE)
    let has_gpt = (0..4).any(|i| {
        let e = 446 + i * 16;
        e + 16 <= data.len() && data[e + 4] == 0xEE
    });

    if has_gpt {
        return parse_gpt(data);
    }

    parse_mbr(data)
}

fn parse_mbr(data: &[u8]) -> Vec<Partition> {
    let mut p = vec![];
    for i in 0..4 {
        let e = 446 + i * 16;
        if e + 16 > data.len() {
            break;
        }
        let pt = data[e + 4];
        if pt == 0 {
            continue;
        }
        let start = u32::from_le_bytes([data[e + 8], data[e + 9], data[e + 10], data[e + 11]])
            as u64;
        let count = u32::from_le_bytes([
            data[e + 12],
            data[e + 13],
            data[e + 14],
            data[e + 15],
        ]) as u64;
        let tn = match pt {
            0x01 => "FAT12",
            0x04 => "FAT16",
            0x06 => "FAT16B",
            0x07 => "NTFS/exFAT",
            0x0b => "FAT32",
            0x0c => "FAT32 LBA",
            0x0e => "FAT16 LBA",
            0x82 => "swap",
            0x83 => "Linux",
            0x8e => "LVM",
            0xee => "GPT",
            0xef => "EFI",
            _ => "?",
        };
        let byte_offset = start * 512;
        let byte_length = count * 512;
        p.push(Partition {
            number: (i + 1) as u8,
            name: format!("P{}", i + 1),
            start_sector: start,
            num_sectors: count,
            type_guid: format!("{:02X}", pt),
            fs_type: tn.into(),
            byte_offset,
            byte_length,
        });
    }
    p
}

fn parse_gpt(data: &[u8]) -> Vec<Partition> {
    let mut parts = vec![];

    if data.len() < 512 + 92 {
        return parts;
    }
    let hdr = &data[512..];
    if &hdr[0..8] != b"EFI PART" {
        return parts;
    }

    let entries_lba = u64::from_le_bytes([
        hdr[72], hdr[73], hdr[74], hdr[75], hdr[76], hdr[77], hdr[78], hdr[79],
    ]);
    let num_entries = u32::from_le_bytes([hdr[80], hdr[81], hdr[82], hdr[83]]) as usize;
    let entry_size = u32::from_le_bytes([hdr[84], hdr[85], hdr[86], hdr[87]]) as usize;

    let entries_offset = (entries_lba * 512) as usize;
    if entries_offset + num_entries * entry_size > data.len() || entry_size < 128 {
        return parts;
    }

    for i in 0..num_entries {
        let off = entries_offset + i * 128;
        if off + 128 > data.len() {
            break;
        }
        let entry = &data[off..off + 128];

        let first_lba = u64::from_le_bytes([
            entry[32], entry[33], entry[34], entry[35],
            entry[36], entry[37], entry[38], entry[39],
        ]);
        let last_lba = u64::from_le_bytes([
            entry[40], entry[41], entry[42], entry[43],
            entry[44], entry[45], entry[46], entry[47],
        ]);
        if first_lba == 0 || last_lba == 0 {
            continue;
        }
        let num_sectors = last_lba - first_lba + 1;
        let byte_offset = first_lba * 512;
        let byte_length = num_sectors * 512;

        let type_guid = guid_to_type(&entry[0..16]);
        let fs_type = type_guid_to_fs(&type_guid);

        let name_bytes = &entry[56..128];
        let name = decode_utf16le_name(name_bytes);
        let name = if name.is_empty() {
            format!("P{}", i + 1)
        } else {
            name
        };

        parts.push(Partition {
            number: (i + 1) as u8,
            name,
            start_sector: first_lba,
            num_sectors,
            type_guid,
            fs_type,
            byte_offset,
            byte_length,
        });
    }

    parts
}

// ── GPT GUID helpers ───────────────────────────────────────────────────

fn guid_to_type(guid: &[u8]) -> String {
    if guid == LINUX_FS_GUID {
        return "Linux filesystem".into();
    }
    if guid == EFI_SYSTEM_GUID {
        return "EFI System".into();
    }
    if guid == LINUX_SWAP_GUID {
        return "Linux swap".into();
    }
    if guid == LINUX_LVM_GUID {
        return "Linux LVM".into();
    }
    if guid == LINUX_HOME_GUID {
        return "Linux /home".into();
    }
    if guid == WINDOWS_BASIC_GUID {
        return "Windows basic".into();
    }
    format_guid(guid)
}

fn format_guid(guid: &[u8]) -> String {
    if guid.len() < 16 {
        return "?".into();
    }
    let t1 = u32::from_le_bytes([guid[0], guid[1], guid[2], guid[3]]);
    let t2 = u16::from_le_bytes([guid[4], guid[5]]);
    let t3 = u16::from_le_bytes([guid[6], guid[7]]);
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        t1, t2, t3, guid[8], guid[9], guid[10], guid[11], guid[12], guid[13], guid[14], guid[15]
    )
}

fn type_guid_to_fs(t: &str) -> String {
    match t {
        "Linux filesystem" | "Linux /home" => "ext4/xfs/btrfs/...".into(),
        "EFI System" => "FAT32".into(),
        "Linux swap" => "swap".into(),
        "Linux LVM" => "LVM2".into(),
        "Windows basic" => "NTFS".into(),
        _ => String::new(),
    }
}

fn decode_utf16le_name(bytes: &[u8]) -> String {
    let mut chars = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let code = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if code == 0 {
            break;
        }
        if let Some(c) = char::from_u32(code as u32) {
            chars.push(c);
        }
        i += 2;
    }
    chars.into_iter().collect()
}

static LINUX_FS_GUID: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D,
    0xE4,
];
static EFI_SYSTEM_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9,
    0x3B,
];
static LINUX_SWAP_GUID: [u8; 16] = [
    0x6D, 0xFD, 0x57, 0x06, 0xAB, 0xA4, 0xC4, 0x43, 0x84, 0xE5, 0x09, 0x33, 0xC8, 0x4B, 0x4F,
    0x4F,
];
static LINUX_LVM_GUID: [u8; 16] = [
    0x79, 0xD3, 0xD6, 0xE6, 0x07, 0xF5, 0xC2, 0x44, 0xA2, 0x3C, 0x23, 0x8F, 0x2A, 0x3D, 0xF9,
    0x28,
];
static LINUX_HOME_GUID: [u8; 16] = [
    0xE1, 0xC7, 0x3A, 0x93, 0xB4, 0x2E, 0x13, 0x4F, 0xB8, 0x44, 0x0E, 0x14, 0xE2, 0xAE, 0xF9,
    0x15,
];
static WINDOWS_BASIC_GUID: [u8; 16] = [
    0x16, 0xE3, 0xC9, 0xE3, 0x5C, 0x0B, 0xB8, 0x4D, 0x81, 0x7D, 0xF9, 0x2D, 0xF0, 0x02, 0x15,
    0xAE,
];

/// Detect filesystem superblock signature at the start of a block
/// This is called at the beginning of each partition's data
pub fn detect_fs_at(block: &[u8]) -> String {
    if block.len() < 1088 {
        return String::new();
    }
    
    // ext2/3/4: superblock at offset 1024, magic at 1080-1081 = 0xEF53
    if block.len() >= 1082 && block[1024+56] == 0x53 && block[1024+57] == 0xEF {
        return "ext4".into();
    }
    
    // XFS: magic "XFSB" at offset 0
    if &block[0..4] == b"XFSB" {
        return "XFS".into();
    }
    
    // btrfs: magic "_BHRf" at offset 0 (actually "_BHR" in first 4 bytes)
    if &block[0..4] == b"_BHR" {
        return "btrfs".into();
    }
    
    // LVM2: "LVM2" at offset 0
    if &block[0..4] == b"LVM2" {
        return "LVM2".into();
    }
    
    // NTFS: "NTFS    " at offset 1080
    if &block[1080..1088] == b"NTFS    " {
        return "NTFS".into();
    }
    
    // FAT: "MSDOS" or "FAT32   " in boot sector at offset 3-7 or 82-90
    if &block[3..8] == b"MSDOS" || &block[82..90] == b"FAT32   " {
        return "FAT".into();
    }
    
    String::new()
}
