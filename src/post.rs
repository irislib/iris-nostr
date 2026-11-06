//! CLI text input and recovery files. Signing and relay transport stay in the library.
use anyhow::{Context, Result, ensure};
use iris_nostr::signing::{Draft, MAX_DRAFT_BYTES};
use nostr::Event;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn read_text(reader: impl Read) -> Result<Draft> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_DRAFT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_DRAFT_BYTES, "text exceeds 1 MiB");
    let content = String::from_utf8(bytes).context("text must be UTF-8")?;
    ensure!(!content.trim().is_empty(), "text must not be empty");
    Ok(Draft {
        kind: 1,
        content,
        tags: Vec::new(),
        created_at: None,
        pubkey: None,
        id: None,
    })
}

fn recovery_directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/share"))
        })
        .context("set XDG_DATA_HOME or HOME, or specify --save-signed")?;
    let directory = base.join("iris-nostr/signed");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&directory)
        .context("create signed event directory")?;
    Ok(directory)
}

/// Publish a complete recovery file without overwriting any existing path.
/// Sync the file and directory before the caller can start networking.
pub fn save_signed(event: &Event, path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match path {
        Some(path) => std::path::absolute(path)?,
        None => recovery_directory()?.join(format!("{}.json", event.id)),
    };
    let parent = path
        .parent()
        .context("signed event file needs a parent directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).context("create signed event file")?;
    serde_json::to_writer(&mut file, event)?;
    file.write_all(b"\n")?;
    file.as_file()
        .sync_all()
        .context("sync signed event file")?;
    file.persist_noclobber(&path)
        .map_err(|error| error.error)
        .context("save signed event; choose a new --save-signed file (existing files are never overwritten)")?;
    sync_directory(parent)?;
    Ok(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)?
        .sync_all()
        .context("sync signed event directory")
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
