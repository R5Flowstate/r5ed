//! Writing an RPak: the manifest the packer reads, the guid a path hashes to,
//! and the checks a built archive has to pass before anything deploys it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The container version every packer writes.
pub const VERSION: u32 = 8;
/// The bit the native pak loader requires on every client pak; without it the
/// engine refuses the file with `pak load failed`.
pub const FLAG_NATIVE_LOADER: u32 = 0x20;
/// Encoded flags the native loader cannot read back.
pub const FLAG_RTECH_ENCODED: u32 = 0x0100;
pub const FLAG_OODLE_ENCODED: u32 = 0x0200;
pub const FLAG_ZSTD_ENCODED: u32 = 0x8000;
/// Paks go out uncompressed: the only build-time compression the packer offers
/// sets an encoding flag this engine cannot decode.
pub const COMPRESS_LEVEL: u32 = 0;
/// Container magic.
pub const MAGIC: [u8; 4] = *b"RPak";
/// Bytes of header the counts live in.
pub const HEADER_BYTES: usize = 0x80;
/// Offset of the asset count in the header.
const ASSET_COUNT_AT: usize = 0x58;
/// Upper bound on files one manifest may name, so a generated manifest cannot
/// turn into an unbounded command line.
pub const MAX_FILES: usize = 8192;

/// One asset in a manifest. Extra keys the packer understands are carried
/// through untouched, which is how streaming hints travel with the asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(rename = "_type")]
    pub kind: String,
    #[serde(rename = "_path")]
    pub path: String,
    #[serde(rename = "$guid", skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Entry {
    /// An asset whose guid the packer derives from the path.
    pub fn new(kind: &str, path: &str) -> Self {
        Self { kind: kind.to_string(), path: path.to_string(), guid: None, extra: Default::default() }
    }

    /// An asset pinned to one guid, which is what a load path is looked up by.
    pub fn pinned(kind: &str, path: &str, guid: u64) -> Self {
        Self {
            kind: kind.to_string(),
            path: path.to_string(),
            guid: Some(format!("0x{guid:016X}")),
            extra: Default::default(),
        }
    }
}

/// A packer manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub name: String,
    #[serde(rename = "assetsDir")]
    pub assets_dir: String,
    #[serde(rename = "outputDir")]
    pub output_dir: String,
    #[serde(rename = "headerFlags")]
    pub header_flags: u32,
    #[serde(rename = "compressLevel")]
    pub compress_level: u32,
    /// The starpak the models' streamed vertex groups go in, as the archive names it.
    #[serde(rename = "streamFileMandatory", default, skip_serializing_if = "Option::is_none")]
    pub stream_file_mandatory: Option<String>,
    pub files: Vec<Entry>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a manifest may name at most {MAX_FILES} assets, not {0}")]
    TooManyFiles(usize),
    #[error("a manifest with no assets packs an empty archive")]
    Empty,
    #[error("asset path {0:?} is not a forward-slashed relative path")]
    BadPath(String),
    #[error("asset path {0:?} is longer than 512 characters")]
    PathTooLong(String),
    #[error("{0} is not a character a path may hold")]
    BadChar(String),
    #[error("could not start {tool}: {source}")]
    Spawn {
        tool: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{tool} exited with {status}: {output}")]
    Exit { tool: PathBuf, status: String, output: String },
    #[error("{path} was not written")]
    Missing { path: PathBuf },
    #[error("{path} is {size} bytes, not an RPak")]
    NotAPak { path: PathBuf, size: u64 },
    #[error("{path} is {size} bytes, too short to hold a header")]
    Short { path: PathBuf, size: u64 },
    #[error("{path} carries flags 0x{flags:04X}, which the native loader refuses")]
    Flags { path: PathBuf, flags: u16 },
    #[error("{path} declares version {version}, not {VERSION}")]
    Version { path: PathBuf, version: u16 },
    #[error("{path} packs no assets")]
    NoAssets { path: PathBuf },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Longest asset path a manifest may name.
const MAX_PATH: usize = 512;

/// A path a manifest may name: forward slashed, relative, no traversal.
pub fn clean_path(path: &str) -> Result<String, Error> {
    if path.is_empty() {
        return Err(Error::BadPath(path.to_string()));
    }
    if path.len() > MAX_PATH {
        return Err(Error::PathTooLong(path.to_string()));
    }
    if path.starts_with('/') || path.contains("..") || path.contains('\\') || path.contains(':') {
        return Err(Error::BadPath(path.to_string()));
    }
    if let Some(c) = path.chars().find(|c| c.is_control() || *c == '\0') {
        return Err(Error::BadChar(c.to_string()));
    }
    if path.split('/').any(|p| p.is_empty() || p == ".") {
        return Err(Error::BadPath(path.to_string()));
    }
    Ok(path.to_string())
}

impl Manifest {
    /// A manifest over a staging directory, with the flags a client pak needs.
    pub fn new(name: &str, assets_dir: &Path, output_dir: &Path) -> Self {
        let dir = |p: &Path| format!("{}/", p.to_string_lossy().replace('\\', "/"));
        Self {
            version: VERSION,
            name: name.to_string(),
            assets_dir: dir(assets_dir),
            output_dir: dir(output_dir),
            header_flags: FLAG_NATIVE_LOADER,
            compress_level: COMPRESS_LEVEL,
            stream_file_mandatory: None,
            files: Vec::new(),
        }
    }

    /// Names the starpak the archive streams from: `paks/Win64/<name>.starpak`.
    pub fn stream_from_own_starpak(&mut self) {
        self.stream_file_mandatory = Some(format!("paks/Win64/{}.starpak", self.name));
    }

    /// The starpak file the packer writes beside the archive, when there is one.
    pub fn starpak(&self) -> Option<PathBuf> {
        let name = self.stream_file_mandatory.as_deref()?;
        let file = name.rsplit(['/', '\\']).next().filter(|f| !f.is_empty())?;
        Some(Path::new(self.output_dir.trim_end_matches('/')).join(file))
    }

    /// Adds an asset, checking the path and the count.
    pub fn push(&mut self, entry: Entry) -> Result<&Entry, Error> {
        let clean = clean_path(&entry.path)?;
        if self.files.len() >= MAX_FILES {
            return Err(Error::TooManyFiles(self.files.len() + 1));
        }
        self.files.push(Entry { path: clean, ..entry });
        Ok(self.files.last().expect("just pushed"))
    }

    /// The archive this manifest writes.
    pub fn output(&self) -> PathBuf {
        Path::new(self.output_dir.trim_end_matches('/')).join(format!("{}.rpak", self.name))
    }

    /// Writes the manifest next to the staging directory.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        if self.files.is_empty() {
            return Err(Error::Empty);
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut body = serde_json::to_string_pretty(self)?;
        body.push('\n');
        std::fs::write(path, body)?;
        Ok(())
    }
}

/// What a built archive's header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub flags: u16,
    pub assets: u32,
}

/// Reads a built archive's header and checks it against what a client pak needs.
pub fn check(path: &Path) -> Result<Header, Error> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < HEADER_BYTES {
        return Err(Error::Short { path: path.to_path_buf(), size: bytes.len() as u64 });
    }
    if bytes[..4] != MAGIC {
        return Err(Error::NotAPak { path: path.to_path_buf(), size: bytes.len() as u64 });
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let header = Header { version: u16_at(4), flags: u16_at(6), assets: u32_at(ASSET_COUNT_AT) };
    if header.version != VERSION as u16 {
        return Err(Error::Version { path: path.to_path_buf(), version: header.version });
    }
    let flags = u32::from(header.flags);
    if flags & FLAG_NATIVE_LOADER == 0 || flags & (FLAG_RTECH_ENCODED | FLAG_OODLE_ENCODED | FLAG_ZSTD_ENCODED) != 0 {
        return Err(Error::Flags { path: path.to_path_buf(), flags: header.flags });
    }
    if header.assets == 0 {
        return Err(Error::NoAssets { path: path.to_path_buf() });
    }
    Ok(header)
}

/// Runs the packer over a manifest and checks what it produced. The argument
/// vector is the program and one path, with no shell in between.
pub fn pack(tool: &Path, manifest: &Manifest, manifest_path: &Path) -> Result<Header, Error> {
    let built = manifest.output();
    // The packer reuses the stream offsets of any archive it finds at the output
    // path and appends to the starpak there, without checking the data matches.
    let stale = [built.clone(), PathBuf::from(format!("{}.pre_cbremap", built.display()))];
    for path in stale.iter().chain(manifest.starpak().iter()) {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(Error::Io(e)),
            _ => {}
        }
    }
    run_packer(tool, manifest_path)?;
    if !built.is_file() {
        return Err(Error::Missing { path: built });
    }
    if let Some(starpak) = manifest.starpak() {
        if !starpak.is_file() {
            return Err(Error::Missing { path: starpak });
        }
    }
    check(&built)
}

/// The zstd level a dedicated-server pak is written at. The server skips an
/// uncompressed map pak without a word, so it is never written without one.
pub const SERVER_COMPRESS_LEVEL: u32 = 6;

/// The manifest of a dedicated-server pak: the packer writes each model in the
/// server's own version and leaves out everything only a client draws with.
pub fn server_manifest(name: &str, assets_dir: &Path, output_dir: &Path, files: &[Entry]) -> serde_json::Value {
    let dir = |p: &Path| format!("{}/", p.to_string_lossy().replace('\\', "/"));
    serde_json::json!({
        "version": VERSION,
        "dedi": true,
        "keepDevOnly": false,
        "keepClientOnly": false,
        "name": name,
        "assetsDir": dir(assets_dir),
        "outputDir": dir(output_dir),
        "compressLevel": SERVER_COMPRESS_LEVEL,
        "files": files,
    })
}

/// Packs a dedicated-server manifest and checks the archive it wrote.
pub fn pack_server(tool: &Path, manifest_path: &Path, built: &Path) -> Result<Header, Error> {
    run_packer(tool, manifest_path)?;
    if !built.is_file() {
        return Err(Error::Missing { path: built.to_path_buf() });
    }
    check_server(built)
}

/// Reads a dedicated-server archive's header: the container version, the zstd
/// encoding the server needs, and at least one asset.
pub fn check_server(path: &Path) -> Result<Header, Error> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < HEADER_BYTES {
        return Err(Error::Short { path: path.to_path_buf(), size: bytes.len() as u64 });
    }
    if bytes[..4] != MAGIC {
        return Err(Error::NotAPak { path: path.to_path_buf(), size: bytes.len() as u64 });
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let header = Header { version: u16_at(4), flags: u16_at(6), assets: u32_at(ASSET_COUNT_AT) };
    if header.version != VERSION as u16 {
        return Err(Error::Version { path: path.to_path_buf(), version: header.version });
    }
    if u32::from(header.flags) & FLAG_ZSTD_ENCODED == 0 {
        return Err(Error::Flags { path: path.to_path_buf(), flags: header.flags });
    }
    if header.assets == 0 {
        return Err(Error::NoAssets { path: path.to_path_buf() });
    }
    Ok(header)
}

fn run_packer(tool: &Path, manifest_path: &Path) -> Result<(), Error> {
    let out = Command::new(tool)
        .arg(manifest_path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::Spawn { tool: tool.to_path_buf(), source: e })?;
    if !out.status.success() {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let tail: String = text.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(Error::Exit {
            tool: tool.to_path_buf(),
            status: out.status.to_string(),
            output: tail.trim().to_string(),
        });
    }
    Ok(())
}

/// The engine's asset-name hash, which is what a manifest guid pins and what a
/// load path is looked up by.
pub fn string_to_guid(name: &str) -> Result<u64, Error> {
    let mut bytes: Vec<u8> = name
        .chars()
        .map(|c| u8::try_from(c as u32).map_err(|_| Error::BadChar(c.to_string())))
        .collect::<Result<_, _>>()?;
    // The hash reads four bytes at a time, so the terminator is followed by
    // enough padding for the last over-read.
    bytes.extend_from_slice(&[0u8; 8]);
    let word = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);

    let mut accumulator: u64 = 0;
    let mut at = 0usize;
    let (v5, v12, v11) = loop {
        let w = word(at);
        let v4 = !w & w.wrapping_sub(0x0101_0101) & 0x8080_8080;
        let v5 = v4 ^ v4.wrapping_sub(1);
        let v6 = (v5 & w) ^ 0x5C5C_5C5C;
        let v7 = !v6 & v6.wrapping_sub(0x0101_0101) & 0x8080_8080;
        let mut v8 = v7 & v7.wrapping_neg();
        if v7 != v8 {
            let mut v9: u32 = 0xFF00_0000;
            loop {
                let previous = v9;
                if v9 & v6 == 0 {
                    v8 |= v9 & 0x8080_8080;
                }
                v9 >>= 8;
                if previous < 0x100 {
                    break;
                }
            }
        }
        let v11 = 0x633D_5F1u64.wrapping_mul(accumulator);
        let inner = (v5 & w).wrapping_sub(45u32.wrapping_mul(v8 >> 7)) & 0xDFDF_DFDF;
        let v12 = 0xFB8C_4D96_501u64.wrapping_mul(u64::from(inner)) >> 24;
        if v4 != 0 {
            break (v5, v12, v11);
        }
        at += 4;
        let sum = v11.wrapping_add(v12);
        accumulator = (sum >> 61) ^ sum;
    };
    // The C original takes the highest set bit of the terminator word, and
    // truncates the division toward zero, which for a missing bit is zero.
    let high = if v5 == 0 { 0i64 } else { i64::from(31 - v5.leading_zeros()) };
    let index = u32::try_from(at).unwrap_or(u32::MAX).wrapping_add((high / 8) as u32);
    Ok(v12.wrapping_add(v11).wrapping_sub(0xAE50_2812_AA73_33u64.wrapping_mul(u64::from(index))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        let mut m = Manifest::new("mp_rr_test", Path::new("stage"), Path::new("out"));
        m.push(Entry::pinned("uiia", "image.uiia", 0x1234)).unwrap();
        m
    }

    #[test]
    fn a_manifest_carries_the_flags_a_client_pak_needs() {
        let m = manifest();
        assert_eq!(m.version, 8);
        assert_eq!(m.header_flags, FLAG_NATIVE_LOADER);
        assert_eq!(m.compress_level, 0);
        assert_eq!(m.assets_dir, "stage/");
        assert_eq!(m.output_dir, "out/");
        assert_eq!(m.output().to_string_lossy().replace('\\', "/"), "out/mp_rr_test.rpak");
        let body = serde_json::to_string(&m).unwrap();
        assert!(body.contains("\"_type\":\"uiia\""), "{body}");
        assert!(body.contains("\"_path\":\"image.uiia\""), "{body}");
        assert!(body.contains("\"$guid\":\"0x0000000000001234\""), "{body}");
        let back: Manifest = serde_json::from_str(&body).expect("round trips");
        assert_eq!(back, m);
        assert!(!body.contains("streamFileMandatory"), "no starpak until a model streams: {body}");
        assert!(m.starpak().is_none());
    }

    #[test]
    fn a_streaming_archive_names_its_own_starpak_beside_it() {
        let mut m = manifest();
        m.stream_from_own_starpak();
        let body = serde_json::to_string(&m).unwrap();
        assert!(body.contains("\"streamFileMandatory\":\"paks/Win64/mp_rr_test.starpak\""), "{body}");
        assert_eq!(m.starpak().unwrap().to_string_lossy().replace('\\', "/"), "out/mp_rr_test.starpak");
        let back: Manifest = serde_json::from_str(&body).expect("round trips");
        assert_eq!(back, m);
    }

    #[test]
    fn a_manifest_refuses_paths_that_leave_the_staging_directory() {
        let mut m = Manifest::new("x", Path::new("stage"), Path::new("out"));
        for bad in ["", "/abs", "a/../b", "a\\b", "c:/d", "a//b", "./a", "a/"] {
            assert!(m.push(Entry::new("wrap", bad)).is_err(), "{bad:?} should be refused");
        }
        assert!(clean_path(&"x".repeat(600)).is_err());
        assert_eq!(clean_path("maps/mp_rr_x.bsp").unwrap(), "maps/mp_rr_x.bsp");
        let mut full = Manifest::new("x", Path::new("stage"), Path::new("out"));
        for i in 0..MAX_FILES {
            full.push(Entry::new("wrap", &format!("maps/{i}.bsp"))).expect("within the cap");
        }
        assert!(matches!(full.push(Entry::new("wrap", "maps/over.bsp")), Err(Error::TooManyFiles(_))));
    }

    #[test]
    fn an_empty_manifest_is_refused_rather_than_packed() {
        let m = Manifest::new("x", Path::new("stage"), Path::new("out"));
        let dir = std::env::temp_dir().join(format!("r5ed-pak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(m.save(&dir.join("m.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guid_hashes_match_the_assets_they_are_looked_up_by() {
        // The guid each of these is known to carry in a shipped pak.
        assert_eq!(string_to_guid("ui_image/loadscreens/mp_rr_district_widescreen.rpak").unwrap(), 0xF826_8129_E803_1940);
        assert_eq!(string_to_guid("ui_image/loadscreens/mp_rr_colony_widescreen.rpak").unwrap(), 0xB01E_3461_D195_7788);
        assert_eq!(string_to_guid("ui_image/overviews/mp_rr_district.rpak").unwrap(), 0xE647_4E48_BBD4_991D);
        // A different name is a different guid, and an empty name is legal.
        assert_ne!(string_to_guid("ui_image/overviews/mp_rr_x.rpak").unwrap(), string_to_guid("").unwrap());
        assert!(string_to_guid("caf\u{4e2d}.rpak").is_err(), "a name outside latin-1 is refused");
    }

    #[test]
    fn a_built_archive_is_checked_before_anything_uses_it() {
        let dir = std::env::temp_dir().join(format!("r5ed-pakhdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, flags: u16, version: u16, assets: u32| {
            let mut b = vec![0u8; HEADER_BYTES];
            b[..4].copy_from_slice(&MAGIC);
            b[4..6].copy_from_slice(&version.to_le_bytes());
            b[6..8].copy_from_slice(&flags.to_le_bytes());
            b[ASSET_COUNT_AT..ASSET_COUNT_AT + 4].copy_from_slice(&assets.to_le_bytes());
            let p = dir.join(name);
            std::fs::write(&p, b).unwrap();
            p
        };
        let good = write("good.rpak", 0x20, 8, 1);
        assert_eq!(check(&good).unwrap(), Header { version: 8, flags: 0x20, assets: 1 });
        assert!(matches!(check(&write("no20.rpak", 0x8000, 8, 1)), Err(Error::Flags { .. })));
        assert!(matches!(check(&write("zstd.rpak", 0x8020, 8, 1)), Err(Error::Flags { .. })));
        assert!(matches!(check(&write("oodle.rpak", 0x220, 8, 1)), Err(Error::Flags { .. })));
        assert!(matches!(check(&write("old.rpak", 0x20, 7, 1)), Err(Error::Version { .. })));
        assert!(matches!(check(&write("empty.rpak", 0x20, 8, 0)), Err(Error::NoAssets { .. })));
        let not_a_pak = dir.join("nope.rpak");
        std::fs::write(&not_a_pak, vec![b'x'; HEADER_BYTES]).unwrap();
        assert!(matches!(check(&not_a_pak), Err(Error::NotAPak { .. })));
        let short = dir.join("short.rpak");
        std::fs::write(&short, b"x").unwrap();
        assert!(matches!(check(&short), Err(Error::Short { .. })));
        assert!(check(&dir.join("missing.rpak")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
