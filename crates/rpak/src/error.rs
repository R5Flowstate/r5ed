use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("rsx exited with status {status}: {stderr}")]
    RsxExit { status: i32, stderr: String },
    #[error("could not start rsx at {0}: {1}")]
    RsxSpawn(PathBuf, std::io::Error),
    #[error("rsx wrote no asset list at {0}")]
    NoAssetList(PathBuf),
    #[error("row {row}: column {column} is not a number: {value:?}")]
    CsvNumber { row: usize, column: &'static str, value: String },
    #[error("row {row}: column {column} is not a guid: {value:?}")]
    CsvGuid { row: usize, column: &'static str, value: String },
    #[error("row {row}: expected {expected} columns, found {found}")]
    CsvArity { row: usize, expected: usize, found: usize },
    #[error("csv header must be {expected}, found {found:?}")]
    CsvHeader { expected: String, found: String },
    #[error("unterminated quoted csv field starting at byte {0}")]
    CsvQuote(usize),
    #[error("json: {0}")]
    Json(String),
    #[error("{what} is truncated: need {need} bytes, have {have}")]
    Truncated { what: &'static str, need: usize, have: usize },
    #[error("{what}: {msg}")]
    Malformed { what: &'static str, msg: String },
    #[error("unknown studiohdr v{0}; rpak reads v16 and v17")]
    RmdlVersion(u32),
    #[error("unsupported vertex position encoding {0}")]
    VertexPosition(u64),
    #[error("unsupported index width {0}")]
    IndexWidth(u32),
    #[error("packed normal names axis 3, which the writer never emits")]
    NormalAxis,
    #[error("asset {path:?} is not in the index")]
    NotIndexed { path: String },
}
