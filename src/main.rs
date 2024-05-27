use md5;
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::{fs, io};

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

const CHUNK_SIZE: usize = 8192; // Define chunk size (e.g., 8KB)

fn sync_single_file(conn: &Connection, path: &Path, source: &Path, dest: &Path) -> io::Result<()> {
    if let Ok(metadata) = fs::metadata(path) {
        let modified = metadata.modified()?.elapsed().unwrap_or_default().as_secs();

        let relative_path = path
            .strip_prefix(source)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let dest_path = dest.join(relative_path);

        let mut file_metadata = FileMetadata {
            path: path.to_str().unwrap().to_string(),
            hash: "".to_string(), // Placeholder for hash, we'll calculate in chunks
            modified,
        };

        let mut source_file = File::open(path)?;
        let mut dest_file = File::create(&dest_path)?;

        let stored_metadata = get_metadata(conn, &file_metadata.path);
        let mut source_hasher = md5::Context::new();
        let mut buffer = vec![0; CHUNK_SIZE];

        loop {
            let bytes_read = source_file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }

            source_hasher.consume(&buffer[..bytes_read]);

            if let Some(_) = &stored_metadata {
                let mut dest_buffer = vec![0; CHUNK_SIZE];
                if let Ok(_) = dest_file.read(&mut dest_buffer) {
                    if buffer[..bytes_read] != dest_buffer[..bytes_read] {
                        dest_file.write_all(&buffer[..bytes_read])?;
                    }
                } else {
                    dest_file.write_all(&buffer[..bytes_read])?;
                }
            } else {
                dest_file.write_all(&buffer[..bytes_read])?;
            }
        }

        file_metadata.hash = format!("{:x}", source_hasher.compute());
        store_metadata(conn, &file_metadata)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
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
