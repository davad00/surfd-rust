use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut address: SocketAddr = "127.0.0.1:27166".parse().unwrap();
    let mut data = PathBuf::from("maps").join("records-server");
    let mut maps = std::env::var_os("SURF_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("maps"));
    let mut backup = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "-h" | "--help" => {
                println!(
                    "Usage: surf-records [--bind 127.0.0.1:27166] [--data DIRECTORY] [--maps DIRECTORY]\n       surf-records --data DIRECTORY --backup FILE"
                );
                return Ok(());
            }
            "--bind" => {
                address = args
                    .next()
                    .ok_or("Missing --bind address")?
                    .parse()
                    .map_err(|_| "Invalid --bind address")?
            }
            "--data" => data = PathBuf::from(args.next().ok_or("Missing --data directory")?),
            "--maps" => maps = PathBuf::from(args.next().ok_or("Missing --maps directory")?),
            "--backup" => {
                backup = Some(PathBuf::from(
                    args.next().ok_or("Missing backup file path")?,
                ))
            }
            _ => return Err(format!("Unknown option {flag}")),
        }
    }
    if let Some(destination) = backup {
        bevy_surf::record_service::write_backup(&data, &destination)?;
        println!("Records backup saved to {}", destination.display());
        return Ok(());
    }
    bevy_surf::record_service::serve(address, &data, &maps)
}
