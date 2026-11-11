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

/// A validated single-layer DDS containing the original Source mip chain.
pub struct BcTexture {
    pub format: BcFormat,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub rgb_only: bool,
    dds: Vec<u8>,
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
        if ![width, height]
            .into_iter()
            .all(|side| (4..=16_384).contains(&side) && side % 4 == 0)
            || mip_levels > width.max(height).ilog2() + 1
        {
            return Err("Invalid DDS dimensions or mip count");
        }
        let bytes_per_block = if format == BcFormat::Bc1 { 8 } else { 16 };
        let length = (0..mip_levels)
            .map(|level| {
                let blocks_wide = (width >> level).max(1).div_ceil(4) as usize;
                let blocks_high = (height >> level).max(1).div_ceil(4) as usize;
                blocks_wide * blocks_high * bytes_per_block
            })
            .sum::<usize>();
        if dds.len() - HEADER_BYTES != length {
            return Err("Incomplete or oversized DDS mip chain");
        }
        Ok(Self {
            format,
            width,
            height,
            mip_levels,
            rgb_only: false,
            dds,
        })
    }

    pub fn blocks(&self) -> &[u8] {
        &self.dds[HEADER_BYTES..]
    }

    pub fn into_blocks(mut self) -> Vec<u8> {
        drop(self.dds.drain(..HEADER_BYTES));
        self.dds
    }
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
}
