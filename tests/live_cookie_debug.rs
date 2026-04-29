//! Debug cookie extraction — see exactly what's in Chrome's database.
//!
//! Run:  cargo test --test live_cookie_debug -- --ignored --nocapture

#[test]
#[ignore]
fn debug_cookies() {
    eprintln!("\n========== Cookie extraction debug ==========\n");

    let home = std::env::var("HOME").unwrap();
    let db_path = format!("{home}/Library/Application Support/Google/Chrome/Default/Cookies");
    eprintln!("DB path: {db_path}");
    eprintln!("Exists: {}", std::path::Path::new(&db_path).exists());

    // Copy to temp to avoid lock issues
    let temp = std::env::temp_dir().join("tchat-debug-cookies.db");
    std::fs::copy(&db_path, &temp).expect("copy DB");

    // Also copy WAL if present
    let wal = format!("{db_path}-wal");
    if std::path::Path::new(&wal).exists() {
        let _ = std::fs::copy(&wal, temp.with_extension("db-wal"));
    }
    let shm = format!("{db_path}-shm");
    if std::path::Path::new(&shm).exists() {
        let _ = std::fs::copy(&shm, temp.with_extension("db-shm"));
    }

    let conn = rusqlite::Connection::open_with_flags(
        &temp,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open DB");

    // List all tables
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    eprintln!("\nTables: {:?}", tables);

    // Show columns in cookies table
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(cookies)").unwrap();
        stmt.query_map([], |row| {
            let name: String = row.get(1)?;
            Ok(name)
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    };
    eprintln!("Cookie columns: {:?}", cols);

    // Count total cookies
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM cookies", [], |r| r.get(0))
        .unwrap();
    eprintln!("Total cookies: {count}");

    // Show all google.com cookies
    eprintln!("\nGoogle cookies:");
    let mut stmt = conn
        .prepare(
            "SELECT host_key, name, LENGTH(encrypted_value), LENGTH(value) FROM cookies \
         WHERE host_key LIKE '%google%' ORDER BY host_key, name",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            let host: String = row.get(0)?;
            let name: String = row.get(1)?;
            let enc_len: i64 = row.get(2)?;
            let val_len: i64 = row.get(3)?;
            Ok((host, name, enc_len, val_len))
        })
        .unwrap();

    let mut google_count = 0;
    for row in rows {
        let (host, name, enc_len, val_len) = row.unwrap();
        eprintln!("  {host:30} {name:30} enc={enc_len:5} plain={val_len:5}");
        google_count += 1;
    }
    eprintln!("\nTotal google cookies: {google_count}");

    // Clean up
    let _ = std::fs::remove_file(&temp);
    let _ = std::fs::remove_file(temp.with_extension("db-wal"));
    let _ = std::fs::remove_file(temp.with_extension("db-shm"));
}
