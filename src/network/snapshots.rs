use std::{io, sync::Arc};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use super::{MAX_FRAME_BYTES, invalid_data};

/// The high length bit marks a zlib-encoded roster on connections that opted in.
pub(crate) const COMPRESSED_FRAME: u32 = 1 << 31;
const MIN_SNAPSHOT_BYTES: usize = 1024;

#[derive(Default)]
pub(crate) struct SnapshotEncoder {
    compressor: Option<Compress>,
    frame: Vec<u8>,
}

impl SnapshotEncoder {
    pub fn encode(&mut self, plain: &[u8]) -> Option<Arc<[u8]>> {
        if plain.len() < MIN_SNAPSHOT_BYTES {
            return None;
        }
        let compressor = self
            .compressor
            .get_or_insert_with(|| Compress::new(Compression::fast(), true));
        compressor.reset();
        self.frame.clear();
        self.frame.reserve(plain.len());
        self.frame.extend_from_slice(&[0; 4]);
        let status = compressor
            .compress_vec(&plain[4..], &mut self.frame, FlushCompress::Finish)
            .ok()?;
        if status != Status::StreamEnd || self.frame.len() >= plain.len() {
            return None;
        }
        let header = COMPRESSED_FRAME | (self.frame.len() - 4) as u32;
        self.frame[..4].copy_from_slice(&header.to_be_bytes());
        Some(Arc::from(self.frame.as_slice()))
    }
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
