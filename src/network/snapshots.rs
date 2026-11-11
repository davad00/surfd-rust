use std::{io, sync::Arc};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use super::{MAX_FRAME_BYTES, PlayerInfo, compact_snapshot, invalid_data};

/// The high length bit marks a zlib-encoded roster on connections that opted in.
pub(crate) const COMPRESSED_FRAME: u32 = 1 << 31;
pub(crate) const COMPACT_FRAME: u32 = 1 << 30;
const MIN_SNAPSHOT_BYTES: usize = 1024;
// Level 2 makes better use of repeated roster fields than the fastest level.
const SNAPSHOT_COMPRESSION: Compression = Compression::new(2);

#[derive(Default)]
pub(crate) struct SnapshotEncoder {
    compressor: Option<Compress>,
    frame: Vec<u8>,
    compact_payload: Vec<u8>,
}

impl SnapshotEncoder {
    pub fn encode(&mut self, plain: &[u8]) -> Option<Arc<[u8]>> {
        compress(&mut self.compressor, &mut self.frame, &plain[4..], 0)
    }

    pub fn encode_compact<'a>(
        &mut self,
        epoch: u64,
        count: usize,
        players: impl Iterator<Item = &'a PlayerInfo>,
    ) -> io::Result<Arc<[u8]>> {
        self.compact_payload.clear();
        compact_snapshot::encode(&mut self.compact_payload, epoch, count, players)?;
        if let Some(frame) = compress(
            &mut self.compressor,
            &mut self.frame,
            &self.compact_payload,
            COMPACT_FRAME,
        ) {
            return Ok(frame);
        }
        let header = COMPACT_FRAME | self.compact_payload.len() as u32;
        self.frame.clear();
        self.frame.extend_from_slice(&header.to_be_bytes());
        self.frame.extend_from_slice(&self.compact_payload);
        Ok(Arc::from(self.frame.as_slice()))
    }
}

fn compress(
    compressor: &mut Option<Compress>,
    frame: &mut Vec<u8>,
    payload: &[u8],
    format: u32,
) -> Option<Arc<[u8]>> {
    if payload.len() + 4 < MIN_SNAPSHOT_BYTES {
        return None;
    }
    let compressor = compressor.get_or_insert_with(|| Compress::new(SNAPSHOT_COMPRESSION, true));
    compressor.reset();
    frame.clear();
    frame.reserve(payload.len() + 4);
    frame.extend_from_slice(&[0; 4]);
    let status = compressor
        .compress_vec(payload, frame, FlushCompress::Finish)
        .ok()?;
    if status != Status::StreamEnd || frame.len() >= payload.len() + 4 {
        return None;
    }
    let header = format | COMPRESSED_FRAME | (frame.len() - 4) as u32;
    frame[..4].copy_from_slice(&header.to_be_bytes());
    Some(Arc::from(frame.as_slice()))
}

#[derive(Default)]
pub(super) struct SnapshotDecoder {
    decompressor: Option<Decompress>,
    payload: Vec<u8>,
}

impl SnapshotDecoder {
    pub fn decode(&mut self, compressed: &[u8]) -> io::Result<&[u8]> {
        let decompressor = self
            .decompressor
            .get_or_insert_with(|| Decompress::new(true));
        decompressor.reset(true);
        self.payload.resize(MAX_FRAME_BYTES, 0);
        let status = decompressor
            .decompress(compressed, &mut self.payload, FlushDecompress::Finish)
            .map_err(|_| invalid_data("invalid compressed room message"))?;
        let decoded = decompressor.total_out() as usize;
        if status != Status::StreamEnd
            || decompressor.total_in() as usize != compressed.len()
            || decoded == 0
        {
            return Err(invalid_data("invalid compressed room message length"));
        }
        Ok(&self.payload[..decoded])
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn moving_roster_uses_less_bandwidth_with_the_same_wire_payload() {
        use super::super::{PlayerInfo, PlayerState, Response, RoomIdentity, ServerEvent, encode};

        let players: Vec<_> = (0..64)
            .map(|index| {
                let timestamp = 12_000 + index * 13 % 50;
                let phase = timestamp as f32 * 0.001 + index as f32 * 0.359;
                PlayerInfo {
                    id: u64::from(index + 1),
                    name: format!("Surfer {index}"),
                    state: Some(PlayerState {
                        position: [
                            phase.sin() * 5000.,
                            phase.cos() * 1300.,
                            (phase * 1.1).cos() * 6000.,
                        ],
                        yaw: phase.sin() * 3.,
                        pitch: phase.cos() * 0.5,
                        elapsed: f64::from(phase),
                        finished: false,
                        crouched: false,
                        eye_height: Some(64.),
                        motion: Some([timestamp, 1]),
                    }),
                    spectating: None,
                    identity: Some(RoomIdentity {
                        account_id: Some(i64::from(index + 1)),
                        moderator: false,
                        points: 0,
                        rank: Default::default(),
                    }),
                }
            })
            .collect();
        let plain = encode(&Response::Event {
            epoch: 1,
            event: ServerEvent::Players(players.clone()),
        })
        .unwrap();
        let compressed = SnapshotEncoder::default().encode(&plain).unwrap();
        let previous = zlib(&plain[4..]);
        assert!(compressed.len() < 4 + previous.len() * 9 / 10);
        assert_eq!(
            SnapshotDecoder::default().decode(&compressed[4..]).unwrap(),
            &plain[4..]
        );
        let compact = SnapshotEncoder::default()
            .encode_compact(1, players.len(), players.iter())
            .unwrap();
        assert!(compact.len() < compressed.len() * 4 / 5);
        let header = u32::from_be_bytes(compact[..4].try_into().unwrap());
        assert_eq!(
            header & (COMPACT_FRAME | COMPRESSED_FRAME),
            COMPACT_FRAME | COMPRESSED_FRAME
        );
        let mut decoder = SnapshotDecoder::default();
        assert_eq!(
            compact_snapshot::decode(decoder.decode(&compact[4..]).unwrap()).unwrap(),
            (1, players)
        );
    }

    #[test]
    fn compressed_room_frames_enforce_the_expanded_wire_limit_and_complete_stream() {
        let mut decoder = SnapshotDecoder::default();
        let payload = vec![b'x'; MAX_FRAME_BYTES];
        let encoded = zlib(&payload);
        assert_eq!(decoder.decode(&encoded).unwrap(), payload);
        assert!(decoder.decode(&encoded[..encoded.len() - 1]).is_err());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decoder.decode(&trailing).is_err());
        let oversized = zlib(&vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert!(decoder.decode(&oversized).is_err());
        assert_eq!(
            decoder.decode(&zlib(b"next snapshot")).unwrap(),
            b"next snapshot"
        );
    }
}
