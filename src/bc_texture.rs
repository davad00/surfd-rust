#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcFormat {
    Bc1,
    Bc2,
    Bc3,
}

impl BcFormat {
    pub(crate) fn from_fourcc(code: &[u8]) -> Option<Self> {
        match code {
            b"DXT1" => Some(Self::Bc1),
            b"DXT3" => Some(Self::Bc2),
            b"DXT5" => Some(Self::Bc3),
            _ => None,
        }
    }
}

/// Validated, largest-first block data for a single-layer texture.
pub struct BcTexture {
    pub format: BcFormat,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub rgb_only: bool,
    data: Vec<u8>,
    block_offset: usize,
}

const HEADER_BYTES: usize = 128;

impl BcTexture {
    pub(crate) fn read(dds: Vec<u8>) -> Result<Self, &'static str> {
        if dds.len() < HEADER_BYTES || &dds[..4] != b"DDS " {
            return Err("Invalid DDS header");
        }
        let word = |offset| u32::from_le_bytes(dds[offset..offset + 4].try_into().unwrap());
        if word(4) != 124 || word(76) != 32 || word(80) & 4 == 0 || word(112) != 0 || word(24) > 1 {
            return Err("Expected a single-layer S3TC texture");
        }
        let format = BcFormat::from_fourcc(&dds[84..88]).ok_or("Unsupported DDS format")?;
        let width = word(16);
        let height = word(12);
        let mip_levels = word(28).max(1);
        let length = mip_chain_len(format, width, height, mip_levels)
            .ok_or("Invalid DDS dimensions or mip count")?;
        if dds.len() - HEADER_BYTES != length {
            return Err("Incomplete or oversized DDS mip chain");
        }
        Ok(Self {
            format,
            width,
            height,
            mip_levels,
            rgb_only: false,
            data: dds,
            block_offset: HEADER_BYTES,
        })
    }

    pub(crate) fn from_vtf(texture: &vformats::vtf::Vtf<'_>) -> Option<Self> {
        use vformats::vtf::BcFormat as VtfFormat;

        let raw = texture.raw_bc(0, 0)?;
        let format = match raw.format {
            VtfFormat::Bc1 => BcFormat::Bc1,
            VtfFormat::Bc2 => BcFormat::Bc2,
            VtfFormat::Bc3 => BcFormat::Bc3,
        };
        let mip_levels = u32::from(texture.mip_count());
        let length = mip_chain_len(format, raw.width, raw.height, mip_levels)?;
        if raw.truncated || raw.mips.len() != mip_levels as usize {
            return None;
        }

        let mut data = Vec::with_capacity(length);
        for (level, mip) in raw.mips.iter().rev().enumerate() {
            if mip.width != (raw.width >> level).max(1)
                || mip.height != (raw.height >> level).max(1)
                || mip.data.len() != mip_len(format, mip.width, mip.height)
            {
                return None;
            }
            data.extend_from_slice(mip.data);
        }
        Some(Self {
            format,
            width: raw.width,
            height: raw.height,
            mip_levels,
            // The CPU decoder interprets both VTF BC1 encodings with one-bit alpha.
            rgb_only: false,
            data,
            block_offset: 0,
        })
    }

    pub fn blocks(&self) -> &[u8] {
        &self.data[self.block_offset..]
    }

    pub fn into_blocks(mut self) -> Vec<u8> {
        if self.block_offset != 0 {
            drop(self.data.drain(..self.block_offset));
        }
        self.data
    }
}

fn mip_len(format: BcFormat, width: u32, height: u32) -> usize {
    let bytes_per_block = if format == BcFormat::Bc1 { 8 } else { 16 };
    width.div_ceil(4) as usize * height.div_ceil(4) as usize * bytes_per_block
}

fn mip_chain_len(format: BcFormat, width: u32, height: u32, levels: u32) -> Option<usize> {
    if ![width, height]
        .into_iter()
        .all(|side| (4..=16_384).contains(&side) && side % 4 == 0)
        || levels == 0
        || levels > width.max(height).ilog2() + 1
    {
        return None;
    }
    Some(
        (0..levels)
            .map(|level| mip_len(format, (width >> level).max(1), (height >> level).max(1)))
            .sum(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bc1_mip_bytes_before_upload() {
        let mut data = vec![0; HEADER_BYTES + 32 + 8 + 8 + 8];
        data[..4].copy_from_slice(b"DDS ");
        for (offset, value) in [(4, 124_u32), (12, 8), (16, 8), (28, 4), (76, 32), (80, 4)] {
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        data[84..88].copy_from_slice(b"DXT1");
        for (index, byte) in data[HEADER_BYTES..].iter_mut().enumerate() {
            *byte = index as u8;
        }
        let expected = data[HEADER_BYTES..].to_vec();
        let owned = data.clone();
        let allocation = owned.as_ptr();
        let texture = BcTexture::read(owned).unwrap();
        assert_eq!(texture.format, BcFormat::Bc1);
        assert_eq!(texture.mip_levels, 4);
        assert_eq!(texture.blocks(), expected);
        let blocks = texture.into_blocks();
        assert_eq!(blocks, expected);
        assert_eq!(blocks.as_ptr(), allocation);
        data.pop();
        assert!(BcTexture::read(data).is_err());
    }

    #[test]
    fn vtf_upload_reverses_mips_and_preserves_bc1_alpha_semantics() {
        for (source_format, expected) in [
            (13_i32, BcFormat::Bc1),
            (20, BcFormat::Bc1),
            (14, BcFormat::Bc2),
            (15, BcFormat::Bc3),
        ] {
            let block_size = mip_len(expected, 4, 4);
            let mut fixture = vec![0; 64];
            fixture[..4].copy_from_slice(b"VTF\0");
            fixture[4..8].copy_from_slice(&7_u32.to_le_bytes());
            fixture[12..16].copy_from_slice(&64_u32.to_le_bytes());
            fixture[16..18].copy_from_slice(&8_u16.to_le_bytes());
            fixture[18..20].copy_from_slice(&8_u16.to_le_bytes());
            fixture[52..56].copy_from_slice(&source_format.to_le_bytes());
            fixture[56] = 2;
            fixture[57..61].copy_from_slice(&(-1_i32).to_le_bytes());
            fixture.extend(std::iter::repeat_n(0x11, block_size));
            fixture.extend(std::iter::repeat_n(0x22, block_size * 4));

            let limits = vformats::Limits::default();
            let texture = vformats::vtf::parse(&fixture, &limits).unwrap();
            let blocks = BcTexture::from_vtf(&texture).unwrap();
            assert_eq!(blocks.format, expected);
            assert_eq!(blocks.mip_levels, 2);
            assert!(!blocks.rgb_only);
            let allocation = blocks.blocks().as_ptr();
            let blocks = blocks.into_blocks();
            assert_eq!(blocks.as_ptr(), allocation);
            assert_eq!(blocks.len(), block_size * 5);
            assert!(blocks[..block_size * 4].iter().all(|&byte| byte == 0x22));
            assert!(blocks[block_size * 4..].iter().all(|&byte| byte == 0x11));

            fixture.pop();
            let truncated = vformats::vtf::parse(&fixture, &limits).unwrap();
            assert!(BcTexture::from_vtf(&truncated).is_none());
        }
    }
}
