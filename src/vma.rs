// VMA archive — direct streaming over uncompressed .vma files.
//
// VmaArchive::open()   Fast two-pass: parse header, then walk VMAE extents
//                       by following the extent_size chain.  No scanning,
//                       no buffering of data — each extent reads only the
//                       512-byte header then seeks past data.
//
// VmaDeviceReader       Read + Seek over File + cluster_index.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

// ── Types ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct VmaDevice {
    pub name: String,
    pub size: u64,
    pub dev_id: usize,
}

type ClusterIndex = Vec<(usize, u64, u16)>;

pub struct VmaArchive {
    pub devices: Vec<VmaDevice>,
    pub config: Vec<(String, String)>,
    pub ctime: String,
    path: PathBuf,
    cluster_index: Vec<ClusterIndex>,
}

pub struct VmaDeviceReader {
    file: File,
    device_size: u64,
    cluster_index: ClusterIndex,
    pos: u64,
    /// Cached cluster buffer to avoid per-read allocations
    cache: Option<(u64, Vec<u8>)>, // (cluster_number, data)
}

// ── Constants ─────────────────────────────────────────────────────────

const CLUSTER_SIZE: u64 = 65536;
const VMAE_HEADER_SIZE: usize = 512;
const BLOCKINFO_START: usize = 40;
const BLOCKINFO_ENTRY: usize = 8;
const MAX_BLOCKS_PER_EXTENT: usize = 59;

// ── VmaArchive ────────────────────────────────────────────────────────

impl VmaArchive {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;

        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        file.rewind()?;

        if magic == [0x28, 0xb5, 0x2f, 0xfd] {
            bail!("{} is zstd-compressed.\nDecompress first:  zstd -d {}", path.display(), path.display());
        }
        if magic[0..2] == [0x1f, 0x8b] {
            bail!("{} is gzip-compressed.\nDecompress first:  gunzip {}", path.display(), path.display());
        }
        if &magic[0..4] != b"VMA\0" {
            bail!("{} is not a valid VMA file", path.display());
        }

        // 1. Parse header
        let header = read_vma_header(&mut file)?;
        let (devices, config, ctime) = parse_header_bytes(&header)?;

        let max_dev_id = devices.iter().map(|d| d.dev_id).max().unwrap_or(0);
        let mut cluster_index: Vec<ClusterIndex> = (0..=max_dev_id).map(|_| vec![]).collect();

        // 2. Walk VMAE extent chain (no scanning — follow extent sizes)
        let file_len = file.metadata()?.len();
        let mut pos = (header.len() as u64).next_multiple_of(512);
        let mut hdr = vec![0u8; VMAE_HEADER_SIZE];

        while pos + VMAE_HEADER_SIZE as u64 <= file_len {
            file.seek(SeekFrom::Start(pos))?;
            file.read_exact(&mut hdr)?;

            // Not a VMAE extent → skip one sector
            if &hdr[..4] != b"VMAE" {
                pos += 512;
                continue;
            }

            let block_count = u16::from_be_bytes([hdr[6], hdr[7]]) as usize;
            let extent_size = VMAE_HEADER_SIZE + block_count * 4096;
            let data_base = pos + VMAE_HEADER_SIZE as u64;

            // Parse block_info
            let mut data_offset: usize = 0;
            for i in 0..MAX_BLOCKS_PER_EXTENT {
                let o = BLOCKINFO_START + i * BLOCKINFO_ENTRY;
                if o + BLOCKINFO_ENTRY > VMAE_HEADER_SIZE { break; }

                let binfo = u64::from_be_bytes([
                    hdr[o], hdr[o+1], hdr[o+2], hdr[o+3],
                    hdr[o+4], hdr[o+5], hdr[o+6], hdr[o+7],
                ]);
                let cn = (binfo & 0xffffffff) as usize;
                let b_did = ((binfo >> 32) & 0xff) as usize;
                let mask = (binfo >> 48) as u16;
                if cn == 0 && b_did == 0 && mask == 0 { continue; }

                if b_did <= max_dev_id {
                    cluster_index[b_did].push((cn, data_base + data_offset as u64, mask));
                }
                data_offset += match mask {
                    0 => 0, 0xffff => 65536,
                    _ => (mask.count_ones() as usize) * 4096,
                };
            }

            // Jump to next extent (back-to-back packing)
            pos += extent_size as u64;
        }

        for ci in &mut cluster_index {
            ci.sort_by_key(|(cn, _, _)| *cn);
            ci.dedup_by_key(|(cn, _, _)| *cn);
        }

        Ok(VmaArchive { path: path.to_path_buf(), devices, config, ctime, cluster_index })
    }

    pub fn open_device(&self, dev_idx: usize) -> Result<VmaDeviceReader> {
        let device = self.devices.get(dev_idx).context("bad device index")?;
        let ci = self.cluster_index.get(device.dev_id).cloned().unwrap_or_default();
        VmaDeviceReader::new(File::open(&self.path)?, device.size, ci)
    }
}

// ── VmaDeviceReader ───────────────────────────────────────────────────

impl VmaDeviceReader {
    fn new(file: File, device_size: u64, ci: ClusterIndex) -> Result<Self> {
        Ok(VmaDeviceReader { file, device_size, cluster_index: ci, pos: 0, cache: None })
    }

    /// Read a cluster, using cache if already loaded
    fn read_cluster(&mut self, cn: usize) -> io::Result<Vec<u8>> {
        // Check cache
        if let Some((cached_cn, ref data)) = self.cache {
            if cached_cn == cn as u64 {
                return Ok(data.clone());
            }
        }

        // Load the cluster
        let result = match self.cluster_index.binary_search_by_key(&cn, |(c,_,_)| *c) {
            Ok(idx) => {
                let (_, fo, mask) = self.cluster_index[idx];
                if mask == 0 {
                    vec![0u8; CLUSTER_SIZE as usize]
                } else {
                    let mut blk = vec![0u8; CLUSTER_SIZE as usize];
                    self.file.seek(SeekFrom::Start(fo))?;
                    if mask == 0xffff {
                        self.file.read_exact(&mut blk)?;
                    } else {
                        for bi in 0..16 {
                            if (mask & (1 << bi)) != 0 {
                                self.file.read_exact(&mut blk[bi*4096..(bi+1)*4096])?;
                            }
                        }
                    }
                    blk
                }
            }
            Err(_) => vec![0u8; CLUSTER_SIZE as usize],
        };

        // Cache it
        self.cache = Some((cn as u64, result.clone()));
        Ok(result)
    }
}

impl Read for VmaDeviceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.device_size { return Ok(0); }
        let cn = (self.pos / CLUSTER_SIZE) as usize;
        let off = (self.pos % CLUSTER_SIZE) as usize;
        let max = (self.device_size - self.pos).min(buf.len() as u64) as usize;

        // Use cached cluster read to avoid repeated allocations and seeks
        let data = self.read_cluster(cn)?;

        let n = max.min(data.len().saturating_sub(off));
        buf[..n].copy_from_slice(&data[off..off+n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for VmaDeviceReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::End(o) => self.device_size as i64 + o,
            SeekFrom::Current(o) => self.pos as i64 + o,
        };
        if new < 0 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek")); }
        self.pos = (new as u64).min(self.device_size);
        Ok(self.pos)
    }
}

// ── Header parsing ────────────────────────────────────────────────────

fn read_vma_header(file: &mut File) -> Result<Vec<u8>> {
    file.rewind()?;
    let mut hb = vec![0u8; 60];
    file.read_exact(&mut hb)?;
    let hs = u32::from_be_bytes([hb[56], hb[57], hb[58], hb[59]]) as usize;
    hb.resize(hs, 0);
    file.read_exact(&mut hb[60..])?;
    Ok(hb)
}

fn parse_header_bytes(data: &[u8]) -> Result<(Vec<VmaDevice>, Vec<(String, String)>, String)> {
    if data.len() < 60 || &data[0..4] != b"VMA\0" { bail!("Invalid VMA header"); }
    let ct = i64::from_be_bytes([data[24],data[25],data[26],data[27],data[28],data[29],data[30],data[31]]);
    let bo = u32::from_be_bytes([data[48],data[49],data[50],data[51]]) as usize;
    let bs = u32::from_be_bytes([data[52],data[53],data[54],data[55]]) as usize;

    let mut blobs: HashMap<u32, Vec<u8>> = HashMap::new();
    let be = bo + bs;
    let mut bp = bo + 1;
    while bp + 2 <= be && bp < data.len() {
        let sz = data[bp] as usize + (data[bp+1] as usize)*256;
        let st = bp + 2;
        if st + sz > data.len().min(be) { break; }
        blobs.insert((bp - bo) as u32, data[st..st+sz].to_vec());
        bp += sz + 2;
    }
    let mut config = vec![];
    for i in 0..256 {
        let ni = 2044 + i*4; let di = 2044 + 1024 + i*4;
        if ni + 4 > data.len() || di + 4 > data.len() { break; }
        let nidx = u32::from_be_bytes([data[ni],data[ni+1],data[ni+2],data[ni+3]]);
        let didx = u32::from_be_bytes([data[di],data[di+1],data[di+2],data[di+3]]);
        if nidx == 0 || didx == 0 { continue; }
        let name = blobs.get(&nidx).map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_string()).unwrap_or_default();
        let val = blobs.get(&didx).map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
        config.push((name, val));
    }
    let mut devices = vec![];
    for did in 1..256 {
        let off = 4096 + (did-1)*32;
        if off + 32 > data.len() { break; }
        let ptr = u32::from_be_bytes([data[off],data[off+1],data[off+2],data[off+3]]);
        let size = u64::from_be_bytes([data[off+8],data[off+9],data[off+10],data[off+11],data[off+12],data[off+13],data[off+14],data[off+15]]);
        if size == 0 || ptr == 0 { continue; }
        let name = blobs.get(&ptr).map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_string()).unwrap_or_else(|| format!("device_{}", did));
        devices.push(VmaDevice { name, size, dev_id: did-1 });
    }
    let ctime = chrono::DateTime::from_timestamp(ct,0).map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| ct.to_string());
    Ok((devices, config, ctime))
}
