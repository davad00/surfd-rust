use std::{
    fs::{self, OpenOptions},
    io,
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};

use super::DATABASE_FILE;

/// Write and validate a consistent copy while the records service is live.
pub fn write_backup(data: &Path, destination: &Path) -> Result<(), String> {
    let destination = std::path::absolute(destination).map_err(|error| error.to_string())?;
    let parent = destination.parent().ok_or("Expected a backup file path.")?;
    match destination.symlink_metadata() {
        Ok(_) => return Err("The backup destination already exists.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not inspect the backup destination: {error}")),
    }
    let source =
        Connection::open_with_flags(data.join(DATABASE_FILE), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("Could not open the records database: {error}"))?;
    source
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let initialized: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='best_runs' AND type='table')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not read the records database: {error}"))?;
    if !initialized {
        return Err("The source file is not an initialized records database.".into());
    }

    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(
        ".surf-backup-{:032x}.tmp",
        u128::from_ne_bytes(nonce)
    ));
    let output = temporary
        .to_str()
        .ok_or("The backup path must be valid UTF-8.")?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(
        options
            .open(&temporary)
            .map_err(|error| format!("Could not create the backup file: {error}"))?,
    );

    let result = (|| {
        source
            .execute("VACUUM INTO ?1", [output])
            .map_err(|error| format!("Could not copy the records database: {error}"))?;
        let snapshot = Connection::open_with_flags(&temporary, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("Could not open the backup for verification: {error}"))?;
        let check: String = snapshot
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(|error| format!("Could not verify the backup: {error}"))?;
        if check != "ok" {
            return Err(format!("The backup failed its integrity check: {check}"));
        }
        drop(snapshot);
        OpenOptions::new()
            .write(true)
            .open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("Could not flush the backup file: {error}"))?;
        // Publish in the same directory without overwriting a competing backup.
        fs::hard_link(&temporary, &destination)
            .map_err(|error| format!("Could not publish the backup: {error}"))?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!("The backup exists, but its directory could not be synced: {error}")
        })?;
    Ok(())
}
