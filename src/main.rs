use md5;
use notify::{Config, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
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
    let source = &args[1];
    let destination = &args[2];

    let conn = Connection::open("sync.db").unwrap();
    init_db(&conn);

    let (tx, rx) = channel();
    let mut watcher: RecommendedWatcher = Watcher::new(tx, Config::default())?;
    watcher.watch(Path::new(source), RecursiveMode::Recursive)?;
    watcher.watch(Path::new(destination), RecursiveMode::Recursive)?;

    for res in rx {
        match res {
            Ok(event) => {
                println!("{:?}", event);
                // Handle event here
                if let Some(path) = event.paths.get(0) {
                    sync_files(&conn, path, source, destination)
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

fn sync_files(
    conn: &Connection,
    path: &Path,
    source: &str,
    destination: &str,
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
