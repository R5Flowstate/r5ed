use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("grid is {0}x{1} but {2} samples were supplied")]
    SizeMismatch(u32, u32, usize),
    #[error("grid dimensions must be greater than zero")]
    ZeroSize,
    #[error("cell size must be finite and greater than zero, got {0}")]
    InvalidCellSize(f32),
    #[error("sample ({0}, {1}) is outside a {2}x{3} grid")]
    OutOfBounds(i64, i64, u32, u32),
    #[error("crop rect {0},{1} {2}x{3} does not fit a {4}x{5} grid")]
    CropOutOfBounds(i64, i64, u32, u32, u32, u32),
    #[error("expected {0} bytes for a {1}x{2} f32 grid, got {3}")]
    BadByteLength(usize, u32, u32, usize),
    #[error("expected {0} bytes for a {1}x{2} u8 grid, got {3}")]
    BadByteLengthU8(usize, u32, u32, usize),
    #[error("height range {0}..{1} is empty or not finite")]
    BadRange(f32, f32),
    #[error("heightfield contains a non-finite sample at {0}")]
    NonFinite(usize),
    #[error("generator parameters are inconsistent: {0}")]
    BadParams(&'static str),
    #[error("recipe node {0:?} is not defined")]
    UnknownNode(String),
    #[error("recipe node {0:?} is defined more than once")]
    DuplicateNode(String),
    #[error("recipe output {0:?} names no node")]
    MissingOutput(String),
    #[error("recipe node {0:?} is referenced before it is produced")]
    ForwardReference(String),
    #[error("recipe depends on itself through node {0:?}")]
    RecipeCycle(String),
    #[error("recipe blend has no terms")]
    EmptyBlend,
    #[error("poisson scatter could not place {0} samples within {1} attempts")]
    ScatterStalled(u32, u32),
    #[error("heightmap {0} is not a supported format")]
    UnsupportedHeightmap(&'static str),
    #[error("heightmap declares {0}x{1} which exceeds the sample limit")]
    HeightmapTooLarge(u32, u32),
    #[error("heightmap data is truncated or not a whole number of samples")]
    BadHeightmapBytes(usize),
    #[error("recipe is larger than the {0} node limit")]
    RecipeTooLarge(usize),
    #[error("recipe json is malformed: {0}")]
    BadJson(String),
    #[error("sdf field dimensions must be at least 2, got {0}x{1}x{2}")]
    SdfTooSmall(u32, u32, u32),
    #[error("sdf field is {0}x{1}x{2} voxels which exceeds the limit")]
    SdfTooLarge(u32, u32, u32),
    #[error("sdf voxel size must be finite and greater than zero, got {0}")]
    InvalidVoxelSize(f64),
    #[error("terrain has no chunks at chunk size {0}")]
    NoChunks(u32),
    #[error("decimation exceeded the error bound of {0}")]
    DecimationError(f32),
    #[error("chunk {0},{1} is outside the {2}x{3} chunk grid")]
    BadChunk(i64, i64, u32, u32),
    #[error("layer {0} has no weight map")]
    MissingWeightMap(usize),
    #[error("weight map {0} is {1}x{2} but the heightfield is {3}x{4}")]
    WeightMapSize(usize, u32, u32, u32, u32),
    #[error("stroke is not valid: {0}")]
    BadStroke(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
