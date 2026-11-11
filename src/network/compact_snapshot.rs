use std::{io, str};

use crate::record_protocol::ProgressionRank;

use super::{PlayerInfo, PlayerState, RoomIdentity, invalid_data};

const MAX_PLAYERS: usize = 64;
const MAX_NAME_BYTES: usize = 96;
const STATE: u8 = 1 << 0;
const SPECTATING: u8 = 1 << 1;
const IDENTITY: u8 = 1 << 2;
const EYE_HEIGHT: u8 = 1 << 3;
const MOTION: u8 = 1 << 4;
const FINISHED: u8 = 1 << 5;
const CROUCHED: u8 = 1 << 6;
const ACCOUNT: u8 = 1 << 7;
const MODERATOR: u8 = 1 << 7;

/// Appends an epoch and a full independent roster to the supplied payload.
/// All fixed-width values are little-endian. Names have a one-byte byte length.
pub(super) fn encode<'a>(
    payload: &mut Vec<u8>,
    epoch: u64,
    count: usize,
    mut players: impl Iterator<Item = &'a PlayerInfo>,
) -> io::Result<()> {
    if count > MAX_PLAYERS {
        return Err(invalid_data("compact roster has too many players"));
    }
    let start = payload.len();
    let result = (|| {
        payload.extend_from_slice(&epoch.to_le_bytes());
        payload.push(count as u8);
        for _ in 0..count {
            let player = players
                .next()
                .ok_or_else(|| invalid_data("compact roster has fewer players than declared"))?;
            encode_player(payload, player)?;
        }
        if players.next().is_some() {
            return Err(invalid_data(
                "compact roster has more players than declared",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        payload.truncate(start);
    }
    result
}

fn flag(present: bool, bit: u8) -> u8 {
    if present { bit } else { 0 }
}

fn encode_player(payload: &mut Vec<u8>, player: &PlayerInfo) -> io::Result<()> {
    let name = player.name.as_bytes();
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid_data("compact roster player name is too long"));
    }
    let mut flags = flag(player.spectating.is_some(), SPECTATING);
    if let Some(state) = &player.state {
        flags |= STATE
            | flag(state.eye_height.is_some(), EYE_HEIGHT)
            | flag(state.motion.is_some(), MOTION)
            | flag(state.finished, FINISHED)
            | flag(state.crouched, CROUCHED);
    }
    if let Some(identity) = &player.identity {
        flags |= IDENTITY | flag(identity.account_id.is_some(), ACCOUNT);
    }

    payload.extend_from_slice(&player.id.to_le_bytes());
    payload.push(flags);
    payload.push(name.len() as u8);
    payload.extend_from_slice(name);
    if let Some(state) = &player.state {
        for coordinate in state.position {
            payload.extend_from_slice(&coordinate.to_le_bytes());
        }
        payload.extend_from_slice(&state.yaw.to_le_bytes());
        payload.extend_from_slice(&state.pitch.to_le_bytes());
        payload.extend_from_slice(&state.elapsed.to_le_bytes());
        if let Some(height) = state.eye_height {
            payload.extend_from_slice(&height.to_le_bytes());
        }
        if let Some(motion) = state.motion {
            for value in motion {
                payload.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    if let Some(target) = player.spectating {
        payload.extend_from_slice(&target.to_le_bytes());
    }
    if let Some(identity) = &player.identity {
        if let Some(id) = identity.account_id {
            payload.extend_from_slice(&id.to_le_bytes());
        }
        payload.extend_from_slice(&identity.points.to_le_bytes());
        payload.push(rank_code(identity.rank) | flag(identity.moderator, MODERATOR));
    }
    Ok(())
}

pub(super) fn decode(mut payload: &[u8]) -> io::Result<(u64, Vec<PlayerInfo>)> {
    let epoch = u64::from_le_bytes(take(&mut payload)?);
    let count = usize::from(u8::from_le_bytes(take(&mut payload)?));
    if count > MAX_PLAYERS {
        return Err(invalid_data("compact roster has too many players"));
    }
    let mut players = Vec::with_capacity(count);
    for _ in 0..count {
        players.push(decode_player(&mut payload)?);
    }
    if !payload.is_empty() {
        return Err(invalid_data("compact roster has trailing bytes"));
    }
    Ok((epoch, players))
}

fn take<const N: usize>(payload: &mut &[u8]) -> io::Result<[u8; N]> {
    let (value, rest) = payload
        .split_first_chunk::<N>()
        .ok_or_else(|| invalid_data("truncated compact roster"))?;
    *payload = rest;
    Ok(*value)
}

fn decode_player(payload: &mut &[u8]) -> io::Result<PlayerInfo> {
    let id = u64::from_le_bytes(take(payload)?);
    let flags = u8::from_le_bytes(take(payload)?);
    if (flags & STATE == 0 && flags & (EYE_HEIGHT | MOTION | FINISHED | CROUCHED) != 0)
        || (flags & IDENTITY == 0 && flags & ACCOUNT != 0)
    {
        return Err(invalid_data("invalid compact roster player flags"));
    }
    let name_len = usize::from(u8::from_le_bytes(take(payload)?));
    if name_len > MAX_NAME_BYTES {
        return Err(invalid_data("compact roster player name is too long"));
    }
    let (name, rest) = payload
        .split_at_checked(name_len)
        .ok_or_else(|| invalid_data("truncated compact roster name"))?;
    *payload = rest;
    let name = str::from_utf8(name).map_err(|_| invalid_data("invalid compact roster name"))?;
    let state = if flags & STATE != 0 {
        Some(decode_state(payload, flags)?)
    } else {
        None
    };
    let spectating = if flags & SPECTATING != 0 {
        Some(u64::from_le_bytes(take(payload)?))
    } else {
        None
    };
    let identity = if flags & IDENTITY != 0 {
        Some(decode_identity(payload, flags)?)
    } else {
        None
    };
    Ok(PlayerInfo {
        id,
        name: name.to_owned(),
        state,
        spectating,
        identity,
    })
}

fn decode_state(payload: &mut &[u8], flags: u8) -> io::Result<PlayerState> {
    Ok(PlayerState {
        position: [
            f32::from_le_bytes(take(payload)?),
            f32::from_le_bytes(take(payload)?),
            f32::from_le_bytes(take(payload)?),
        ],
        yaw: f32::from_le_bytes(take(payload)?),
        pitch: f32::from_le_bytes(take(payload)?),
        elapsed: f64::from_le_bytes(take(payload)?),
        finished: flags & FINISHED != 0,
        crouched: flags & CROUCHED != 0,
        eye_height: if flags & EYE_HEIGHT != 0 {
            Some(f32::from_le_bytes(take(payload)?))
        } else {
            None
        },
        motion: if flags & MOTION != 0 {
            Some([
                u32::from_le_bytes(take(payload)?),
                u32::from_le_bytes(take(payload)?),
            ])
        } else {
            None
        },
    })
}

fn decode_identity(payload: &mut &[u8], flags: u8) -> io::Result<RoomIdentity> {
    let account_id = if flags & ACCOUNT != 0 {
        Some(i64::from_le_bytes(take(payload)?))
    } else {
        None
    };
    let points = u32::from_le_bytes(take(payload)?);
    let rank_and_moderator = u8::from_le_bytes(take(payload)?);
    Ok(RoomIdentity {
        account_id,
        moderator: rank_and_moderator & MODERATOR != 0,
        points,
        rank: decode_rank(rank_and_moderator & !MODERATOR)?,
    })
}

fn rank_code(rank: ProgressionRank) -> u8 {
    match rank {
        ProgressionRank::Unranked => 0,
        ProgressionRank::Rookie => 1,
        ProgressionRank::Surfer => 2,
        ProgressionRank::Regular => 3,
        ProgressionRank::Skilled => 4,
        ProgressionRank::Expert => 5,
        ProgressionRank::Master => 6,
        ProgressionRank::Legend => 7,
    }
}

fn decode_rank(code: u8) -> io::Result<ProgressionRank> {
    Ok(match code {
        0 => ProgressionRank::Unranked,
        1 => ProgressionRank::Rookie,
        2 => ProgressionRank::Surfer,
        3 => ProgressionRank::Regular,
        4 => ProgressionRank::Skilled,
        5 => ProgressionRank::Expert,
        6 => ProgressionRank::Master,
        7 => ProgressionRank::Legend,
        _ => return Err(invalid_data("invalid compact roster rank")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(id: u64, name: &str) -> PlayerInfo {
        PlayerInfo {
            id,
            name: name.into(),
            state: None,
            spectating: None,
            identity: None,
        }
    }

    fn encoded(players: &[PlayerInfo]) -> Vec<u8> {
        let mut payload = Vec::new();
        encode(&mut payload, u64::MAX, players.len(), players.iter()).unwrap();
        payload
    }

    #[test]
    fn preserves_signed_guest_and_spectator_rosters_and_float_bits() {
        let mut surfer = player(u64::MAX - 1, "雪 🏄 Élan");
        surfer.state = Some(PlayerState {
            position: [-0.0, f32::from_bits(1), f32::MAX],
            yaw: f32::from_bits(0x3f80_0001),
            pitch: -0.0,
            elapsed: f64::from_bits(0x3ff0_0000_0000_0001),
            finished: true,
            crouched: true,
            eye_height: Some(f32::from_bits(0x4264_0001)),
            motion: Some([u32::MAX, 2]),
        });
        surfer.identity = Some(RoomIdentity {
            account_id: Some(i64::MAX),
            moderator: true,
            points: u32::MAX,
            rank: ProgressionRank::Master,
        });
        let mut spectator = player(7, "Watcher");
        spectator.spectating = Some(surfer.id);
        spectator.identity = Some(RoomIdentity {
            account_id: None,
            moderator: false,
            points: 0,
            rank: ProgressionRank::Unranked,
        });
        let roster = [surfer, player(2, "LAN Guest"), spectator];
        let (epoch, decoded) = decode(&encoded(&roster)).unwrap();
        assert_eq!(epoch, u64::MAX);
        assert_eq!(decoded, roster);
        let expected = roster[0].state.unwrap();
        let actual = decoded[0].state.unwrap();
        assert_eq!(
            actual.position.map(f32::to_bits),
            expected.position.map(f32::to_bits)
        );
        assert_eq!(actual.yaw.to_bits(), expected.yaw.to_bits());
        assert_eq!(actual.pitch.to_bits(), expected.pitch.to_bits());
        assert_eq!(actual.elapsed.to_bits(), expected.elapsed.to_bits());
        assert_eq!(
            actual.eye_height.map(f32::to_bits),
            expected.eye_height.map(f32::to_bits)
        );
    }

    #[test]
    fn enforces_counts_and_utf8_byte_limit_and_keeps_existing_prefix() {
        let boundary = player(1, &"🏄".repeat(24));
        let oversized = player(2, &"🏄".repeat(25));
        let roster = vec![boundary.clone(); 64];
        assert_eq!(decode(&encoded(&roster)).unwrap().1, roster);
        assert!(decode(&encoded(&[])).unwrap().1.is_empty());
        let mut payload = vec![21, 22];
        assert!(encode(&mut payload, 1, 65, roster.iter()).is_err());
        assert!(encode(&mut payload, 1, 2, [&boundary].into_iter()).is_err());
        assert!(encode(&mut payload, 1, 0, [&boundary].into_iter()).is_err());
        assert!(encode(&mut payload, 1, 2, [&boundary, &oversized].into_iter()).is_err());
        assert_eq!(payload, [21, 22]);
        encode(&mut payload, 1, 1, [&boundary].into_iter()).unwrap();
        assert_eq!(&payload[..2], &[21, 22]);
        assert_eq!(decode(&payload[2..]).unwrap().1, [boundary]);
    }

    #[test]
    fn rejects_truncated_trailing_or_malformed_payloads() {
        let mut plain = encoded(&[player(1, "A")]);
        for end in 0..plain.len() {
            assert!(decode(&plain[..end]).is_err());
        }
        plain.push(0);
        assert!(decode(&plain).is_err());
        plain.pop();
        plain[8] = 65;
        assert!(decode(&plain).is_err());
        plain[8] = 1;
        for flag in [EYE_HEIGHT, MOTION, FINISHED, CROUCHED, ACCOUNT] {
            plain[17] = flag;
            assert!(decode(&plain).is_err());
        }
        plain[17] = 0;
        plain[18] = 97;
        assert!(decode(&plain).is_err());
        plain[18] = 1;
        plain[19] = 0xff;
        assert!(decode(&plain).is_err());

        let mut with_identity = player(3, "B");
        with_identity.identity = Some(RoomIdentity {
            account_id: None,
            moderator: false,
            points: 0,
            rank: ProgressionRank::Regular,
        });
        let mut identity = encoded(&[with_identity]);
        *identity.last_mut().unwrap() = 8;
        assert!(decode(&identity).is_err());
    }
}
