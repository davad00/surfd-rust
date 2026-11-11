use std::{
    thread,
    time::{Duration, Instant},
};

use bevy_surf::{
    catalog,
    network::{self, Client, ClientMessage, RoomInfo, ServerEvent},
    record_protocol::ManagedRoomEntry,
};
use serde::{Deserialize, Serialize};

use crate::records_client as api;

#[derive(Deserialize)]
struct RecordsReadiness {
    ready: bool,
}

#[derive(Serialize)]
struct RoomCheck {
    address: String,
    discovery_ms: u128,
    room: RoomInfo,
}

#[derive(Serialize)]
struct RoomStatus {
    reachable: bool,
    full: bool,
    #[serde(flatten)]
    snapshot: RoomCheck,
}

pub(super) fn join_managed_room(
    endpoint: &str,
    entry: &ManagedRoomEntry,
    token: Option<&str>,
) -> Result<Client, String> {
    let ticket = api::post(endpoint, "/v1/room-tickets", token, entry)?;
    Client::connect_with_ticket(&entry.address, ticket)
}

pub(super) fn run(room_id: &str) -> Result<(), String> {
    let endpoint = api::configured_endpoint();
    let records: RecordsReadiness = api::get_fast(&endpoint, "/v1/ready")
        .map_err(|error| format!("Records readiness check failed: {error}"))?;
    if !records.ready {
        return Err("The records service is not ready.".into());
    }
    let (entry, _, latency) = discover_registered_room(&endpoint, room_id)?;
    let client = join_managed_room(&endpoint, &entry, None)
        .map_err(|error| format!("Could not start the guest join: {error}"))?;
    let room = await_welcome(&client)?;
    leave(&client)?;
    if room.room_id.as_deref() != Some(room_id) {
        return Err("The joined room does not match the records service directory.".into());
    }
    if !room.map.eq_ignore_ascii_case("downline") && catalog::find(&room.map).is_none() {
        return Err(format!(
            "The room's current map, {}, is outside this client's catalog.",
            room.map
        ));
    }
    print_json(&RoomCheck {
        address: entry.address,
        discovery_ms: latency.as_millis(),
        room,
    })
}

pub(super) fn status(room_id: &str) -> Result<(), String> {
    let (entry, room, latency) = discover_registered_room(&api::configured_endpoint(), room_id)?;
    print_json(&RoomStatus {
        reachable: true,
        full: room.players >= room.capacity,
        snapshot: RoomCheck {
            address: entry.address,
            discovery_ms: latency.as_millis(),
            room,
        },
    })
}

fn discover_registered_room(
    endpoint: &str,
    room_id: &str,
) -> Result<(ManagedRoomEntry, RoomInfo, Duration), String> {
    let directory: Vec<ManagedRoomEntry> = api::get_fast(endpoint, "/v1/managed-rooms")
        .map_err(|error| format!("Could not read the managed room directory: {error}"))?;
    let entry = directory
        .into_iter()
        .find(|entry| entry.room_id == room_id)
        .ok_or_else(|| format!("The records service has no managed room named {room_id}."))?;
    let (discovered, latency) = network::discover_with_latency(&entry.address)?;
    if discovered.room_id.as_deref() != Some(room_id) {
        return Err("The room identity does not match the records service directory.".into());
    }
    Ok((entry, discovered, latency))
}

fn print_json(result: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(result).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn await_welcome(client: &Client) -> Result<RoomInfo, String> {
    let deadline = Instant::now() + Duration::from_secs(7);
    while Instant::now() < deadline {
        match client.poll() {
            Some(ServerEvent::Welcome { room, .. }) => return Ok(room),
            Some(ServerEvent::Error(reason) | ServerEvent::Disconnected(reason)) => {
                return Err(format!("Guest join failed: {reason}"));
            }
            Some(_) => continue,
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    Err("The room did not finish accepting the guest before the check timed out.".into())
}

fn leave(client: &Client) -> Result<(), String> {
    client.send(ClientMessage::Leave)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match client.poll() {
            Some(ServerEvent::Disconnected(_)) => return Ok(()),
            Some(_) => continue,
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    Err("The room accepted the guest but did not finish disconnecting.".into())
}
