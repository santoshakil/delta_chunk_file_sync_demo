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
                sync_files(&conn, source, destination);
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

fn sync_files(conn: &Connection, source: &str, destination: &str) {
    let source_path = Path::new(source);
    let dest_path = Path::new(destination);

    // Sync source to destination
    sync_directory(conn, source_path, dest_path);
    // Sync destination to source
    sync_directory(conn, dest_path, source_path);
}

fn sync_directory(conn: &Connection, source: &Path, dest: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            let metadata = fs::metadata(&path).unwrap();
            let modified = metadata.modified().unwrap().elapsed().unwrap().as_secs();
            let content = fs::read_to_string(&path).unwrap();
            let hash = format!("{:x}", md5::compute(&content));

            let file_metadata = FileMetadata {
                path: path.to_str().unwrap().to_string(),
                hash: hash.clone(),
                modified,
            };

            let stored_metadata: Option<FileMetadata> = get_metadata(conn, &file_metadata.path);

            if let Some(stored) = stored_metadata {
                if stored.hash != hash {
                    fs::write(&path, content.clone()).unwrap();
                    store_metadata(conn, &file_metadata);
                    let dest_path = path.strip_prefix(source).unwrap().join(dest);
                    fs::write(dest_path, content).unwrap();
                }
            } else {
                store_metadata(conn, &file_metadata);
                let dest_path = path.strip_prefix(source).unwrap().join(dest);
                fs::write(dest_path, content).unwrap();
            }
        }
    }
}

fn store_metadata(conn: &Connection, metadata: &FileMetadata) {
    conn.execute(
        "INSERT OR REPLACE INTO file_metadata (path, hash, modified) VALUES (?1, ?2, ?3)",
        params![metadata.path, metadata.hash, metadata.modified],
    )
    .unwrap();
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
