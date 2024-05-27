use md5;
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

#[derive(Serialize, Deserialize, Debug)]
struct FileMetadata {
    path: String,
    hash: String,
    modified: u64,
}

fn main() -> NotifyResult<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <source> <destination>", args[0]);
        return Ok(());
    }
    let source = std::fs::canonicalize(&args[1])?;
    let destination = std::fs::canonicalize(&args[2])?;

    let conn = Connection::open("sync.db").unwrap();
    init_db(&conn);

    let (tx, rx) = channel();
    let mut watcher: RecommendedWatcher = Watcher::new(tx, Config::default())?;
    watcher.watch(&source, RecursiveMode::Recursive)?;
    watcher.watch(&destination, RecursiveMode::Recursive)?;

    for res in rx {
        match res {
            Ok(event) => {
                if let Some(path) = event.paths.get(0) {
                    handle_event(&conn, &event, path, &source, &destination)
                        .unwrap_or_else(|e| eprintln!("Sync error: {:?}", e));
                }
            }
            Err(e) => println!("watch error: {:?}", e),
        }
    }

    Ok(())
}

fn init_db(conn: &Connection) {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_metadata (
                  path TEXT PRIMARY KEY,
                  hash TEXT NOT NULL,
                  modified INTEGER NOT NULL
                  )",
        [],
    )
    .unwrap();
}

fn store_metadata(conn: &Connection, metadata: &FileMetadata) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO file_metadata (path, hash, modified) VALUES (?1, ?2, ?3)",
        params![metadata.path, metadata.hash, metadata.modified],
    )?;
    Ok(())
}

fn get_metadata(conn: &Connection, path: &str) -> Option<FileMetadata> {
    conn.query_row(
        "SELECT path, hash, modified FROM file_metadata WHERE path = ?1",
        params![path],
        |row| {
            Ok(FileMetadata {
                path: row.get(0)?,
                hash: row.get(1)?,
                modified: row.get(2)?,
            })
        },
    )
    .optional()
    .unwrap()
}

fn handle_event(
    conn: &Connection,
    event: &Event,
    path: &Path,
    source: &PathBuf,
    destination: &PathBuf,
) -> std::io::Result<()> {
    match event.kind {
        EventKind::Modify(_) | EventKind::Create(_) => {
            sync_files(conn, path, source, destination)?;
        }
        EventKind::Remove(_) => {
            // Handle file removal
            remove_file(conn, path, source, destination)?;
        }
        _ => {}
    }
    Ok(())
}

fn sync_files(
    conn: &Connection,
    path: &Path,
    source: &PathBuf,
    destination: &PathBuf,
) -> std::io::Result<()> {
    let source_path = Path::new(source);
    let dest_path = Path::new(destination);

    if path.starts_with(source_path) {
        sync_single_file(conn, path, source_path, dest_path)
    } else if path.starts_with(dest_path) {
        sync_single_file(conn, path, dest_path, source_path)
    } else {
        Ok(())
    }
}

fn sync_single_file(
    conn: &Connection,
    path: &Path,
    source: &Path,
    dest: &Path,
) -> std::io::Result<()> {
    if let Ok(metadata) = fs::metadata(path) {
        let modified = metadata.modified()?.elapsed().unwrap_or_default().as_secs();
        let content = fs::read_to_string(path)?;
        let hash = format!("{:x}", md5::compute(&content));

        let relative_path = path
            .strip_prefix(source)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let dest_path = dest.join(relative_path);

        let file_metadata = FileMetadata {
            path: path.to_str().unwrap().to_string(),
            hash: hash.clone(),
            modified,
        };

        let stored_metadata = get_metadata(conn, &file_metadata.path);

        if let Some(stored) = stored_metadata {
            if stored.hash != hash {
                fs::write(&dest_path, &content)?;
                store_metadata(conn, &file_metadata)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            }
        } else {
            fs::write(&dest_path, &content)?;
            store_metadata(conn, &file_metadata)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        }
    }
    Ok(())
}

fn remove_file(
    conn: &Connection,
    path: &Path,
    source: &PathBuf,
    destination: &PathBuf,
) -> std::io::Result<()> {
    let source_path = Path::new(source);
    let dest_path = Path::new(destination);

    if path.starts_with(source_path) {
        let relative_path = path
            .strip_prefix(source)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let dest_file_path = dest_path.join(relative_path);
        if dest_file_path.exists() {
            fs::remove_file(dest_file_path)?;
        }
    } else if path.starts_with(dest_path) {
        let relative_path = path
            .strip_prefix(dest_path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let source_file_path = source_path.join(relative_path);
        if source_file_path.exists() {
            fs::remove_file(source_file_path)?;
        }
    }

    let file_path = path.to_str().unwrap().to_string();
    conn.execute(
        "DELETE FROM file_metadata WHERE path = ?1",
        params![file_path],
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    Ok(())
}
