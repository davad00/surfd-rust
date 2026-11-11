use bevy_surf::{
    network::{RoomTicketKey, RoomTlsConfig},
    server::{AdminCommand, RoomConfig, RoomControl, RoomServer},
};
use std::{
    io::{self, BufRead},
    path::PathBuf,
    process::ExitCode,
    thread,
    time::Duration,
};

const HELP: &str = "\
Usage: surf-server [--bind IP:PORT] [--name NAME] [--map MAP_ID]
                   [--rotation MAP_ID,MAP_ID] [--capacity N]
                   [--map-minutes N] [--vote-seconds N]
                   [--tls-cert PEM --tls-key PEM] [--moderation-file PATH]

The default bind is 127.0.0.1:27165.
Set SURF_ROOM_ID and SURF_ROOM_TICKET_KEY together for a managed room.
Non-loopback managed rooms require TLS. Join with tls://host:port.
Press Ctrl+C to stop the room and notify connected players.
The local console accepts:
  players                 List player IDs
  kick ID                 Disconnect a player
  mute ID / unmute ID     Change a player's chat permissions
  ban ID / unban IP       Block an address from this room
  unban-account ACCOUNT   Remove a managed account's room ban
  map MAP_ID              Load a map from this room's rotation
  say MESSAGE             Send a room notice
  quit                    Stop the room";

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
    let Some(config) = parse_args()? else {
        println!("{HELP}");
        return Ok(());
    };
    let name = config.name.clone();
    let map = config.map.clone();
    let server =
        RoomServer::bind(config).map_err(|error| format!("Could not start room: {error}"))?;
    let control = server.control();
    let signal_control = control.clone();
    ctrlc::set_handler(move || signal_control.shutdown())
        .map_err(|error| format!("Could not install the room shutdown handler: {error}"))?;
    println!("{name} is listening on {} with {map}.", server.local_addr());
    println!("Enter 'help' for local moderation commands.");
    thread::Builder::new()
        .name("surf-room-console".to_owned())
        .spawn(move || run_console(control))
        .map_err(|error| format!("Could not start the room console: {error}"))?;
    server
        .wait()
        .map_err(|error| format!("Room stopped: {error}"))
}

fn parse_args() -> Result<Option<RoomConfig>, String> {
    let mut config = RoomConfig::default();
    let mut tls_certificate = std::env::var_os("SURF_ROOM_TLS_CERT").map(PathBuf::from);
    let mut tls_key = std::env::var_os("SURF_ROOM_TLS_KEY").map(PathBuf::from);
    config.moderation_file = std::env::var_os("SURF_ROOM_MODERATION_FILE").map(PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if matches!(flag.as_str(), "--help" | "-h") {
            return Ok(None);
        }
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {flag}."))?;
        match flag.as_str() {
            "--bind" => {
                config.bind = value
                    .parse()
                    .map_err(|_| "Enter a bind address like 127.0.0.1:27165.")?;
            }
            "--name" => config.name = value,
            "--map" => config.map = value.trim().to_ascii_lowercase(),
            "--rotation" => {
                config.rotation = value
                    .split(',')
                    .map(str::trim)
                    .map(str::to_ascii_lowercase)
                    .collect();
            }
            "--capacity" => {
                config.capacity = value.parse().map_err(|_| "Invalid player capacity.")?
            }
            "--map-minutes" => {
                let minutes: u64 = value.parse().map_err(|_| "Invalid map duration.")?;
                config.map_duration = Duration::from_secs(minutes.saturating_mul(60));
            }
            "--vote-seconds" => {
                config.vote_duration =
                    Duration::from_secs(value.parse().map_err(|_| "Invalid vote duration.")?);
            }
            "--tls-cert" => tls_certificate = Some(PathBuf::from(value)),
            "--tls-key" => tls_key = Some(PathBuf::from(value)),
            "--moderation-file" => config.moderation_file = Some(PathBuf::from(value)),
            _ => return Err(format!("Unknown option {flag}.")),
        }
    }
    config.managed = match (
        std::env::var("SURF_ROOM_ID"),
        std::env::var("SURF_ROOM_TICKET_KEY"),
    ) {
        (Ok(id), Ok(key)) => Some(RoomTicketKey::from_hex(&id, &key)?),
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => None,
        _ => {
            return Err(
                "Set both SURF_ROOM_ID and SURF_ROOM_TICKET_KEY for a managed room.".into(),
            );
        }
    };
    config.tls = match (tls_certificate, tls_key) {
        (Some(certificate), Some(key)) => Some(
            RoomTlsConfig::from_pem(&certificate, &key)
                .map_err(|error| format!("Could not load room TLS certificate and key: {error}"))?,
        ),
        (None, None) => None,
        _ => return Err("Provide both a TLS certificate and its private key.".into()),
    };
    if config.moderation_file.is_none()
        && let Some(managed) = &config.managed
    {
        config.moderation_file = Some(
            std::env::var_os("SURF_DATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("maps"))
                .join("room-data")
                .join(format!("{}.json", managed.room_id())),
        );
    }
    Ok(Some(config))
}

fn run_console(control: RoomControl) {
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "help" | "?") {
            println!("{HELP}");
            continue;
        }
        if matches!(line, "quit" | "stop") {
            control.shutdown();
            break;
        }
        if let Err(error) = parse_command(line).and_then(|command| control.send(command)) {
            eprintln!("{error}");
        }
    }
}

fn parse_command(line: &str) -> Result<AdminCommand, String> {
    let (command, argument) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(command, argument)| (command, argument.trim()));
    let id = || {
        argument
            .parse::<u64>()
            .map_err(|_| "Enter a player ID from 'players'.".to_owned())
    };
    match command {
        "players" => Ok(AdminCommand::Players),
        "kick" => Ok(AdminCommand::Kick(id()?)),
        "mute" => Ok(AdminCommand::Mute(id()?)),
        "unmute" => Ok(AdminCommand::Unmute(id()?)),
        "ban" => Ok(AdminCommand::Ban(id()?)),
        "unban" => Ok(AdminCommand::Unban(
            argument
                .parse()
                .map_err(|_| "Enter an IPv4 or IPv6 address.")?,
        )),
        "unban-account" => Ok(AdminCommand::UnbanAccount(
            argument
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| "Enter a positive records account ID.".to_owned())?,
        )),
        "map" if !argument.is_empty() => Ok(AdminCommand::ChangeMap(argument.to_ascii_lowercase())),
        "say" if !argument.is_empty() => Ok(AdminCommand::Announce(argument.to_owned())),
        _ => Err("Unknown or incomplete console command. Enter 'help'.".to_owned()),
    }
}
