use std::{fs, path::Path};

use anyhow::{Context, Result};
use bytes::Bytes;
use fastcdc::v2020::FastCDC;
use sha2::{Digest, Sha256};

pub const MIN_CHUNK_SIZE: u32 = 524_288;
pub const AVG_CHUNK_SIZE: u32 = 1_048_576;
pub const MAX_CHUNK_SIZE: u32 = 8_388_608;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub hash: String,
    pub offset: u64,
    pub length: u64,
    pub data: Bytes,
}

pub fn chunk_file(path: &Path) -> Result<Vec<Chunk>> {
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let source = Bytes::from(data);
    let chunker = FastCDC::new(&source, MIN_CHUNK_SIZE, AVG_CHUNK_SIZE, MAX_CHUNK_SIZE);
    let mut chunks = Vec::new();

    for chunk in chunker {
        let offset = chunk.offset as usize;
        let length = chunk.length as usize;
        let bytes = source.slice(offset..offset + length);
        let hash = hex::encode(Sha256::digest(&bytes));

        chunks.push(Chunk {
            hash,
            offset: chunk.offset as u64,
            length: chunk.length as u64,
            data: bytes,
        });
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn manifest_sum_equals_file_size() {
        let data = vec![7u8; 5 * 1024 * 1024];
        let file = write_temp(&data);

        let chunks = chunk_file(file.path()).unwrap();
        let total = chunks.iter().map(|chunk| chunk.length).sum::<u64>();

        assert_eq!(total, data.len() as u64);
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].offset + pair[0].length, pair[1].offset);
        }
    }

    #[test]
    fn small_file_produces_single_chunk() {
        let data = b"hello, valv";
        let file = write_temp(data);

        let chunks = chunk_file(file.path()).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].length, data.len() as u64);
    }

    #[test]
    fn same_file_chunks_identically() {
        let data = vec![42u8; 2 * 1024 * 1024];
        let file = write_temp(&data);

        let a = chunk_file(file.path()).unwrap();
        let b = chunk_file(file.path()).unwrap();

        assert_eq!(a, b);
    }
}
