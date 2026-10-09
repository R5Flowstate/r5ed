use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub const IDENT: [u8; 4] = *b"rBSP";
pub const LUMP_COUNT: usize = 128;
pub const HEADER_SIZE: usize = 16 + LUMP_COUNT * 16;
/// Every shipped Respawn map declares 127 here, one less than the table size.
pub const DECLARED_LUMP_COUNT: u32 = 127;
/// The five partition files a map may carry beside its `.bsp`.
pub const PARTITION_NAMES: &[&str] = &["env", "fx", "script", "snd", "spawn"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LumpEntry {
    pub offset: u32,
    /// The engine trusts this value, not the sidecar size. A few lumps (RTL
    /// pages) declare more bytes than they store.
    pub length: u32,
    pub version: u32,
    pub fourcc: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub ident: [u8; 4],
    pub version: u16,
    pub flags: u16,
    pub revision: u32,
    pub lump_count: u32,
    pub lumps: [LumpEntry; LUMP_COUNT],
}

impl Header {
    pub fn new(version: u16, flags: u16, revision: u32) -> Self {
        Self {
            ident: IDENT,
            version,
            flags,
            revision,
            lump_count: DECLARED_LUMP_COUNT,
            lumps: [LumpEntry::default(); LUMP_COUNT],
        }
    }

    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < HEADER_SIZE {
            return Err(Error::Truncated { what: "header", need: HEADER_SIZE, have: b.len() });
        }
        let ident: [u8; 4] = b[0..4].try_into().unwrap();
        if ident != IDENT {
            return Err(Error::BadIdent(ident));
        }
        let u16_at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let mut lumps = [LumpEntry::default(); LUMP_COUNT];
        for (i, l) in lumps.iter_mut().enumerate() {
            let o = 16 + i * 16;
            *l = LumpEntry { offset: u32_at(o), length: u32_at(o + 4), version: u32_at(o + 8), fourcc: u32_at(o + 12) };
        }
        Ok(Self { ident, version: u16_at(4), flags: u16_at(6), revision: u32_at(8), lump_count: u32_at(12), lumps })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_SIZE);
        b.extend_from_slice(&self.ident);
        b.extend_from_slice(&self.version.to_le_bytes());
        b.extend_from_slice(&self.flags.to_le_bytes());
        b.extend_from_slice(&self.revision.to_le_bytes());
        b.extend_from_slice(&self.lump_count.to_le_bytes());
        for l in &self.lumps {
            for v in [l.offset, l.length, l.version, l.fourcc] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }
}

/// How the lump bytes are stored on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Header plus all lump data in one file at the header offsets.
    Monolithic,
    /// `<name>.bsp` holds the header only; lump N lives in `<name>.bsp.<nnnn>.bsp_lump`.
    Sidecar,
}

/// A map as the engine sees it: the header table verbatim plus each lump's
/// bytes. Reading then writing with the same layout is byte-identical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bsp {
    pub header: Header,
    pub lumps: BTreeMap<u16, Vec<u8>>,
    pub layout: Layout,
    /// The sidecar file name each lump was read from, so a caller can tell that
    /// an uppercase name missed the engine's case-insensitive lookup.
    pub sidecar_names: BTreeMap<u16, String>,
    /// Size of the packed file a monolithic read came from. Bytes past the last
    /// lump are preserved when writing back.
    pub packed_size: usize,
}

impl Bsp {
    pub fn new(version: u16, flags: u16, revision: u32) -> Self {
        Self {
            header: Header::new(version, flags, revision),
            lumps: BTreeMap::new(),
            layout: Layout::Sidecar,
            sidecar_names: BTreeMap::new(),
            packed_size: 0,
        }
    }

    pub fn version(&self) -> u16 {
        self.header.version
    }

    pub fn lump(&self, id: u16) -> &[u8] {
        self.lumps.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Replaces a lump and sets its declared length to the stored size.
    pub fn set_lump(&mut self, id: u16, data: Vec<u8>) {
        self.set_lump_declared(id, data, None);
    }

    /// Replaces a lump with an explicit declared length, for the lumps whose
    /// header length is not the stored size.
    pub fn set_lump_declared(&mut self, id: u16, data: Vec<u8>, declared: Option<u32>) {
        let e = &mut self.header.lumps[id as usize];
        e.length = declared.unwrap_or(data.len() as u32);
        if data.is_empty() && declared.is_none() {
            e.offset = 0;
            self.lumps.remove(&id);
        } else {
            self.lumps.insert(id, data);
        }
    }

    pub fn remove_lump(&mut self, id: u16) {
        self.header.lumps[id as usize] = LumpEntry::default();
        self.lumps.remove(&id);
    }

    /// The version family this map's records use, if it is one we know.
    pub fn version_kind(&self) -> Option<crate::Version> {
        crate::Version::from_header(self.header.version)
    }

    pub fn from_monolithic(b: &[u8]) -> Result<Self> {
        let header = Header::parse(b)?;
        let mut lumps = BTreeMap::new();
        for (i, l) in header.lumps.iter().enumerate() {
            if l.length == 0 {
                continue;
            }
            let (o, n) = (l.offset as usize, l.length as usize);
            let end = o.checked_add(n).ok_or(Error::LumpRange { id: i as u16 })?;
            if o < HEADER_SIZE || end > b.len() {
                return Err(Error::LumpRange { id: i as u16 });
            }
            lumps.insert(i as u16, b[o..end].to_vec());
        }
        Ok(Self {
            header,
            lumps,
            layout: Layout::Monolithic,
            sidecar_names: BTreeMap::new(),
            packed_size: b.len(),
        })
    }

    /// Lays every lump out in index order, each 4-byte aligned after the header.
    /// A map read from a packed file keeps its original offsets instead, and
    /// only a lump whose bytes changed is placed afresh.
    pub fn to_monolithic(&self) -> Vec<u8> {
        if self.layout == Layout::Monolithic && self.packed_size >= HEADER_SIZE {
            return self.repack_preserving_offsets();
        }
        let mut header = self.header.clone();
        let mut body: Vec<u8> = Vec::new();
        for id in 0..LUMP_COUNT as u16 {
            let Some(data) = self.lumps.get(&id) else {
                // A slot with a declared length but no stored bytes cannot be
                // packed: clear it so the result is a self-consistent file.
                header.lumps[id as usize] = LumpEntry::default();
                continue;
            };
            while !(HEADER_SIZE + body.len()).is_multiple_of(4) {
                body.push(0);
            }
            header.lumps[id as usize].offset = (HEADER_SIZE + body.len()) as u32;
            header.lumps[id as usize].length = data.len() as u32;
            body.extend_from_slice(data);
        }
        // The game lump's block pointers are absolute in a packed file, so they
        // have to follow the offset the lump just landed on.
        if let Some(rebased) = self.rebase_game_lump(&header) {
            let at = header.lumps[crate::ids::GAME_LUMP as usize].offset as usize;
            let from = at - HEADER_SIZE;
            body[from..from + rebased.len()].copy_from_slice(&rebased);
            header.lumps[crate::ids::GAME_LUMP as usize].length = rebased.len() as u32;
        }
        let mut out = header.to_bytes();
        out.extend_from_slice(&body);
        out
    }

    /// Re-encodes lump 0x23 with absolute block pointers, when the map's own
    /// pointers are not already absolute against the offset it now sits at.
    fn rebase_game_lump(&self, header: &Header) -> Option<Vec<u8>> {
        let id = crate::ids::GAME_LUMP;
        let data = self.lumps.get(&id)?;
        let old = self.header.lumps[id as usize].offset;
        let new = header.lumps[id as usize].offset;
        crate::lumps::game_lump::rebase(data, old, new).ok().flatten()
    }

    /// Replays the offsets a packed file was read with. A lump whose stored
    /// length no longer matches what the header declared is appended instead,
    /// so nothing else moves and the padding between lumps survives.
    fn repack_preserving_offsets(&self) -> Vec<u8> {
        let mut header = self.header.clone();
        let mut out: Vec<u8> = vec![0; self.packed_size];
        out[..HEADER_SIZE].copy_from_slice(&header.to_bytes());
        let mut tail = self.packed_size;
        for (id, data) in &self.lumps {
            let e = &mut header.lumps[*id as usize];
            let fits = e.length as usize == data.len()
                && e.offset as usize >= HEADER_SIZE
                && e.offset as usize + data.len() <= self.packed_size;
            if fits {
                let o = e.offset as usize;
                out[o..o + data.len()].copy_from_slice(data);
            } else {
                while !tail.is_multiple_of(4) {
                    tail += 1;
                }
                out.resize(tail + data.len(), 0);
                out[tail..tail + data.len()].copy_from_slice(data);
                e.offset = tail as u32;
                e.length = data.len() as u32;
                tail += data.len();
            }
        }
        if let Some(rebased) = self.rebase_game_lump(&header) {
            let at = header.lumps[crate::ids::GAME_LUMP as usize].offset as usize;
            out[at..at + rebased.len()].copy_from_slice(&rebased);
            header.lumps[crate::ids::GAME_LUMP as usize].length = rebased.len() as u32;
        }
        out[..HEADER_SIZE].copy_from_slice(&header.to_bytes());
        out
    }

    pub fn sidecar_path(bsp_path: &Path, id: u16) -> PathBuf {
        let mut s = bsp_path.as_os_str().to_owned();
        s.push(format!(".{:04x}.bsp_lump", id));
        PathBuf::from(s)
    }

    /// The name of a lump's sidecar, lowercase, as `write_path` emits it.
    pub fn sidecar_name(bsp_name: &str, id: u16) -> String {
        format!("{bsp_name}.{id:04x}.bsp_lump")
    }

    /// True when a sidecar file name is all lowercase hex.
    pub fn sidecar_name_is_lowercase(name: &str) -> bool {
        match name.rsplit_once(".bsp_lump") {
            Some((head, _)) => {
                let Some(hex) = head.rsplit_once('.').map(|(_, h)| h) else { return false };
                hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }
            None => false,
        }
    }

    /// Reads `path`. A header-only file (exactly the header size) is read as
    /// sidecar layout; sidecar names are matched in either hex case.
    pub fn read_path(path: &Path) -> Result<Self> {
        let b = fs::read(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
        if b.len() > HEADER_SIZE {
            return Self::from_monolithic(&b);
        }
        let header = Header::parse(&b)?;
        let mut lumps = BTreeMap::new();
        let mut names = BTreeMap::new();
        for id in 0..LUMP_COUNT as u16 {
            let lower = Self::sidecar_path(path, id);
            let mut s = path.as_os_str().to_owned();
            s.push(format!(".{:04X}.bsp_lump", id));
            let upper = PathBuf::from(s);
            let p = if lower.is_file() {
                lower
            } else if upper.is_file() {
                upper
            } else {
                continue;
            };
            let data = fs::read(&p).map_err(|e| Error::Io(p.clone(), e))?;
            names.insert(id, p.file_name().unwrap_or_default().to_string_lossy().to_string());
            lumps.insert(id, data);
        }
        Ok(Self { header, lumps, layout: Layout::Sidecar, sidecar_names: names, packed_size: 0 })
    }

    /// Reads a map and the partition files beside it, keyed by partition name.
    /// The `.bsp` is looked for in `dir` and then in `dir/maps`, and the
    /// partition files are read from whichever holds it. The name list comes
    /// from the 0x18 lump, falling back to the five names the engine looks for.
    pub fn read_dir_map(dir: &Path, stem: &str) -> Result<(Self, BTreeMap<String, Vec<u8>>)> {
        let direct = dir.join(format!("{stem}.bsp"));
        let (path, home) = if direct.is_file() {
            (direct, dir.to_path_buf())
        } else {
            let nested = dir.join("maps").join(format!("{stem}.bsp"));
            if nested.is_file() {
                let p = nested.parent().unwrap_or(dir).to_path_buf();
                (nested, p)
            } else {
                return Err(Error::Io(direct, std::io::Error::from(std::io::ErrorKind::NotFound)));
            }
        };
        let bsp = Self::read_path(&path)?;
        let mut out = BTreeMap::new();
        let mut names: Vec<String> = Vec::new();
        if let Ok(l) = crate::lumps::parse(
            crate::ids::ENTITY_PARTITIONS,
            bsp.version_kind().unwrap_or(crate::Version::ClientV51),
            bsp.lump(crate::ids::ENTITY_PARTITIONS),
        ) {
            if let Some(p) = l.as_entity_partitions() {
                names = p.names.clone();
            }
        }
        if names.is_empty() {
            for n in PARTITION_NAMES {
                if home.join(format!("{stem}_{n}.ent")).is_file() {
                    names.push((*n).to_string());
                }
            }
        }
        for n in names {
            let file = home.join(format!("{stem}_{n}.ent"));
            if !file.is_file() {
                continue;
            }
            let data = fs::read(&file).map_err(|e| Error::Io(file.clone(), e))?;
            out.insert(n, data);
        }
        Ok((bsp, out))
    }

    /// Writes `path` in `self.layout`. Sidecar names are always lowercase hex:
    /// the engine filesystem lowercases the lookup and misses uppercase names.
    /// Stale sidecars of the same map are removed first.
    pub fn write_path(&self, path: &Path) -> Result<()> {
        match self.layout {
            Layout::Monolithic => fs::write(path, self.to_monolithic()).map_err(|e| Error::Io(path.to_path_buf(), e)),
            Layout::Sidecar => {
                if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
                    let prefix = format!("{}.", name.to_string_lossy());
                    if let Ok(rd) = fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { dir }) {
                        for e in rd.flatten() {
                            let f = e.file_name().to_string_lossy().to_string();
                            if f.starts_with(&prefix) && f.to_ascii_lowercase().ends_with(".bsp_lump") {
                                let _ = fs::remove_file(e.path());
                            }
                        }
                    }
                }
                for (id, data) in &self.lumps {
                    let p = Self::sidecar_path(path, *id);
                    fs::write(&p, data).map_err(|e| Error::Io(p.clone(), e))?;
                }
                fs::write(path, self.header.to_bytes()).map_err(|e| Error::Io(path.to_path_buf(), e))
            }
        }
    }
}
