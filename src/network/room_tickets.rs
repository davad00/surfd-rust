use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::record_protocol::ProgressionRank;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const TTL_SECONDS: i64 = 60;
const MAX_CLOCK_SKEW: i64 = 5;
const MAX_TICKET_LEN: usize = 1024;
const TICKET_VERSION: u8 = 1;
const SIGNATURE_DOMAIN: &[u8] = b"surfd-native/room-ticket/v1\0";
const PROOF_KEY_DOMAIN: &[u8] = b"surfd-native/room-proof-key/v1\0";
const CHALLENGE_DOMAIN: &[u8] = b"surfd-native/room-challenge/v1\0";
const SERVER_PROOF_DOMAIN: &[u8] = b"surfd-native/server-challenge/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomIdentity {
    pub account_id: Option<i64>,
    pub moderator: bool,
    #[serde(default)]
    pub points: u32,
    #[serde(default)]
    pub rank: ProgressionRank,
}

#[derive(Deserialize, Serialize)]
pub struct RoomJoinTicket {
    ticket: String,
    proof_key: [u8; 32],
}

impl RoomJoinTicket {
    pub(super) fn respond(
        &self,
        challenge: &[u8; 32],
        server_proof: &[u8; 32],
    ) -> Option<[u8; 32]> {
        verifier(&self.proof_key, SERVER_PROOF_DOMAIN, challenge)
            .verify_slice(server_proof)
            .ok()?;
        Some(sign(&self.proof_key, CHALLENGE_DOMAIN, challenge))
    }

    pub(super) fn token(&self) -> &str {
        &self.ticket
    }
}

#[derive(Clone)]
pub struct RoomTicketKey {
    room_id: String,
    secret: [u8; 32],
}

impl fmt::Debug for RoomTicketKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoomTicketKey")
            .field("room_id", &self.room_id)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Serialize)]
pub(crate) struct TicketClaims {
    version: u8,
    room: String,
    pub nonce: [u8; 16],
    pub name: String,
    pub identity: RoomIdentity,
    issued: i64,
    pub expires: i64,
}

impl RoomTicketKey {
    pub fn from_hex(room_id: &str, secret: &str) -> Result<Self, String> {
        if !(1..=32).contains(&room_id.len())
            || !room_id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            return Err(
                "Managed room IDs use 1 to 32 lowercase letters, digits, hyphens or underscores."
                    .into(),
            );
        }
        if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Managed room ticket keys must be 32 random bytes in hexadecimal.".into());
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&secret[index * 2..index * 2 + 2], 16).expect("validated hex");
        }
        if bytes.iter().all(|byte| *byte == bytes[0]) {
            return Err("Use a randomly generated managed room ticket key.".into());
        }
        Ok(Self {
            room_id: room_id.into(),
            secret: bytes,
        })
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    #[cfg(feature = "records-service")]
    pub(crate) fn shares_secret_with(&self, other: &Self) -> bool {
        self.secret == other.secret
    }

    #[cfg(any(feature = "records-service", test))]
    pub(crate) fn issue(
        &self,
        account: Option<(i64, String, bool, u32)>,
        now: i64,
    ) -> Result<RoomJoinTicket, String> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let (name, identity) = if let Some((id, name, moderator, points)) = account {
            (
                name,
                RoomIdentity {
                    account_id: Some(id),
                    moderator,
                    points,
                    rank: ProgressionRank::from_points(points),
                },
            )
        } else {
            (
                format!("Guest {:04X}", u16::from_be_bytes([nonce[0], nonce[1]])),
                RoomIdentity {
                    account_id: None,
                    moderator: false,
                    points: 0,
                    rank: ProgressionRank::Unranked,
                },
            )
        };
        let claims = TicketClaims {
            version: TICKET_VERSION,
            room: self.room_id.clone(),
            nonce,
            name,
            identity,
            issued: now,
            expires: now + TTL_SECONDS,
        };
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|error| error.to_string())?);
        let signature =
            URL_SAFE_NO_PAD.encode(sign(&self.secret, SIGNATURE_DOMAIN, payload.as_bytes()));
        let ticket = format!("{payload}.{signature}");
        let proof_key = self.proof_key(&ticket);
        Ok(RoomJoinTicket { ticket, proof_key })
    }

    pub(crate) fn verify(&self, ticket: &str, now: i64) -> Option<TicketClaims> {
        if ticket.len() > MAX_TICKET_LEN {
            return None;
        }
        let (payload, signature) = ticket.split_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        verifier(&self.secret, SIGNATURE_DOMAIN, payload.as_bytes())
            .verify_slice(&signature)
            .ok()?;
        let claims: TicketClaims =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        if claims.version != TICKET_VERSION
            || claims.room != self.room_id
            || claims.issued > now + MAX_CLOCK_SKEW
            || claims.expires <= now
            || claims.expires.checked_sub(claims.issued)? != TTL_SECONDS
            || claims.identity.account_id.is_some_and(|id| id <= 0)
            || (claims.identity.account_id.is_none()
                && (claims.identity.moderator || claims.identity.points != 0))
            || claims.identity.rank != ProgressionRank::from_points(claims.identity.points)
        {
            return None;
        }
        Some(claims)
    }

    pub(crate) fn proof_key(&self, ticket: &str) -> [u8; 32] {
        sign(&self.secret, PROOF_KEY_DOMAIN, ticket.as_bytes())
    }
}

pub(crate) fn verify_room_proof(key: &[u8; 32], challenge: &[u8; 32], proof: &[u8; 32]) -> bool {
    verifier(key, CHALLENGE_DOMAIN, challenge)
        .verify_slice(proof)
        .is_ok()
}

pub(crate) fn room_server_proof(key: &[u8; 32], challenge: &[u8; 32]) -> [u8; 32] {
    sign(key, SERVER_PROOF_DOMAIN, challenge)
}

pub(crate) fn ticket_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn verifier(key: &[u8; 32], domain: &[u8], payload: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts 32-byte keys");
    mac.update(domain);
    mac.update(payload);
    mac
}

fn sign(key: &[u8; 32], domain: &[u8], payload: &[u8]) -> [u8; 32] {
    verifier(key, domain, payload)
        .finalize()
        .into_bytes()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = "982188bce302ba52a05d6fa070bbf97ec93773e50e20e5a228ad11e580ef5156";

    #[test]
    fn ticket_requires_room_signature_and_short_validity() {
        let signer = RoomTicketKey::from_hex("eu", TEST_KEY).unwrap();
        let other = RoomTicketKey::from_hex("na", TEST_KEY).unwrap();
        let ticket = signer
            .issue(Some((15, "Alice".into(), true, 2000)), 100)
            .unwrap();
        let claims = signer.verify(ticket.token(), 105).unwrap();
        assert_eq!(claims.name, "Alice");
        assert_eq!(
            claims.identity,
            RoomIdentity {
                account_id: Some(15),
                moderator: true,
                points: 2000,
                rank: ProgressionRank::Regular
            }
        );
        assert!(signer.verify(ticket.token(), 94).is_none());
        assert!(signer.verify(ticket.token(), 160).is_none());
        assert!(other.verify(ticket.token(), 105).is_none());
        let tampered = format!("x{}", &ticket.token()[1..]);
        assert!(signer.verify(&tampered, 105).is_none());
        let key = signer.proof_key(ticket.token());
        let proof = ticket
            .respond(&[1; 32], &room_server_proof(&key, &[1; 32]))
            .unwrap();
        assert!(verify_room_proof(&key, &[1; 32], &proof));
        assert!(!verify_room_proof(&key, &[2; 32], &proof));
        assert!(ticket.respond(&[1; 32], &[0; 32]).is_none());
    }
}
