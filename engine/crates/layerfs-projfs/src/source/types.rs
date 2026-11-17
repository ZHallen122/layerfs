//! A source's entries, listings, errors and the trait a mount projects.
/// 100 ns ticks between 1601-01-01 and the Unix epoch.
pub const EPOCH_TICKS: i64 = 116_444_736_000_000_000;

/// Windows file-time ticks of a Unix time.
pub fn ticks(seconds: i64, nanoseconds: u32) -> i64 {
    seconds
        .saturating_mul(10_000_000)
        .saturating_add(i64::from(nanoseconds / 100))
        .saturating_add(EPOCH_TICKS)
}

/// The Unix time of Windows file-time ticks.
pub fn unix(ticks: i64) -> (i64, u32) {
    let since = ticks - EPOCH_TICKS;
    (
        since.div_euclid(10_000_000),
        (since.rem_euclid(10_000_000) * 100) as u32,
    )
}

/// One entry of a source directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
    /// A file's length in bytes; 0 for a directory.
    pub size: u64,
    /// Last write time, in 100 ns ticks since 1601.
    pub written: i64,
    /// Windows attribute bits; `DIRECTORY` agrees with `directory`.
    pub attributes: u32,
    /// The source's identity of a file's bytes, at most 128 bytes. The
    /// provider stores it in the placeholder and hands it back to
    /// [`Source::read`], so a read needs no second lookup and stays correct
    /// after the source's view of the path changed.
    pub content: Vec<u8>,
}

/// One directory's entries, in any order.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// Names the source holds that a Windows mount cannot show, each with
    /// its reason. They stay in the source untouched.
    pub unprojected: Vec<(String, &'static str)>,
}

/// Why a source read failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceError {
    /// No such directory or file.
    NotFound,
    /// The source failed; the text is its own cause.
    Failed(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Failed(text) => f.write_str(text),
        }
    }
}

/// An immutable tree a mount projects.
pub trait Source: Send + Sync + 'static {
    /// The entries of the directory at `directory`.
    fn list(&self, directory: &str) -> Result<Listing, SourceError>;
    /// Fills `buffer` with the file's bytes from `offset`. `content` is the
    /// identity its entry carried; `path` is where the entry was listed.
    fn read(
        &self,
        path: &str,
        content: &[u8],
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), SourceError>;
}
