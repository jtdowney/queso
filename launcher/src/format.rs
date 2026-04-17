use std::io::{Read, Seek, SeekFrom, Write};

use bincode::{Decode, Encode};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use eyre::{Result, bail, ensure};

pub const TRAILER_MAGIC: &[u8; 8] = b"QUESO\x00\x01\x02";
pub const TRAILER_SIZE: usize = size_of::<Trailer>() + TRAILER_MAGIC.len();

#[derive(Debug, Clone, Encode, Decode)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    pub entry_module: String,
    pub erts_version: String,
    pub erts_hash: String,
    pub app_hash: String,
    pub boot_path: String,
}

impl Metadata {
    pub fn validate(&self) -> Result<()> {
        validate_path_component("name", &self.name)?;
        validate_path_component("version", &self.version)?;
        validate_path_component("erts_version", &self.erts_version)?;
        validate_entry_module(&self.entry_module)?;
        validate_boot_path(&self.boot_path)?;
        Ok(())
    }
}

fn validate_path_component(field: &str, s: &str) -> Result<()> {
    ensure!(!s.is_empty(), "metadata: {field} is empty");
    ensure!(s != "." && s != "..", "metadata: {field} is '.' or '..'");
    ensure!(
        !s.bytes().any(|b| b == b'/' || b == b'\\'),
        "metadata: {field} contains path separator"
    );

    Ok(())
}

fn validate_entry_module(s: &str) -> Result<()> {
    ensure!(!s.is_empty(), "metadata: entry_module is empty");
    ensure!(
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'@'),
        "metadata: entry_module contains disallowed characters"
    );

    Ok(())
}

fn validate_boot_path(s: &str) -> Result<()> {
    ensure!(!s.is_empty(), "metadata: boot_path is empty");
    ensure!(!s.starts_with('/'), "metadata: boot_path must be relative");
    ensure!(
        !s.split(['/', '\\']).any(|c| c == ".." || c == "."),
        "metadata: boot_path contains '.' or '..' component"
    );

    Ok(())
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
#[allow(clippy::struct_field_names)]
pub struct Trailer {
    pub erts_offset: u64,
    pub app_offset: u64,
    pub meta_offset: u64,
}

impl Trailer {
    #[allow(dead_code)]
    pub fn write<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_u64::<LittleEndian>(self.erts_offset)?;
        writer.write_u64::<LittleEndian>(self.app_offset)?;
        writer.write_u64::<LittleEndian>(self.meta_offset)?;
        writer.write_all(TRAILER_MAGIC)?;
        Ok(())
    }

    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        let Some(trailer_pos) = file_len.checked_sub(TRAILER_SIZE as u64) else {
            bail!("file too small to contain trailer");
        };

        reader.seek(SeekFrom::Start(trailer_pos))?;

        let erts_offset = reader.read_u64::<LittleEndian>()?;
        let app_offset = reader.read_u64::<LittleEndian>()?;
        let meta_offset = reader.read_u64::<LittleEndian>()?;

        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;

        if magic != *TRAILER_MAGIC {
            bail!("invalid binary: missing queso magic trailer");
        }

        Ok(Self {
            erts_offset,
            app_offset,
            meta_offset,
        })
    }

    pub fn validate(&self, file_len: u64) -> Result<()> {
        let trailer_size = TRAILER_SIZE as u64;
        ensure!(
            file_len >= trailer_size,
            "binary too small to contain trailer"
        );
        ensure!(
            self.erts_offset < self.app_offset,
            "trailer: erts_offset must be less than app_offset"
        );
        ensure!(
            self.app_offset < self.meta_offset,
            "trailer: app_offset must be less than meta_offset"
        );
        let Some(meta_end) = self.meta_offset.checked_add(trailer_size) else {
            bail!("trailer: meta_offset + trailer size overflows");
        };
        ensure!(
            meta_end <= file_len,
            "trailer: metadata extends past end of file"
        );

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::io::{Cursor, Write};

    use byteorder::WriteBytesExt;

    use super::*;

    pub(super) fn write_trailer_bytes(trailer: &Trailer, out: &mut Vec<u8>) {
        out.write_u64::<LittleEndian>(trailer.erts_offset).unwrap();
        out.write_u64::<LittleEndian>(trailer.app_offset).unwrap();
        out.write_u64::<LittleEndian>(trailer.meta_offset).unwrap();
        out.write_all(TRAILER_MAGIC).unwrap();
    }

    #[test]
    fn test_trailer_size() {
        assert_eq!(TRAILER_SIZE, 3 * size_of::<u64>() + TRAILER_MAGIC.len());
    }

    #[test]
    fn test_trailer_roundtrip() {
        let trailer = Trailer {
            erts_offset: 1024,
            app_offset: 2048,
            meta_offset: 4096,
        };

        let mut buf = Vec::with_capacity(TRAILER_SIZE);
        write_trailer_bytes(&trailer, &mut buf);

        let mut cursor = Cursor::new(&buf);

        let read_back = Trailer::read(&mut cursor).unwrap();
        assert_eq!(read_back.erts_offset, 1024);
        assert_eq!(read_back.app_offset, 2048);
        assert_eq!(read_back.meta_offset, 4096);
    }

    #[test]
    fn test_trailer_invalid_magic() {
        let mut buf = vec![0u8; TRAILER_SIZE];

        let mut cursor = Cursor::new(&mut buf);
        cursor.write_u64::<LittleEndian>(100).unwrap();
        cursor.write_u64::<LittleEndian>(200).unwrap();
        cursor.write_u64::<LittleEndian>(300).unwrap();
        cursor.write_all(b"NOTQUESO").unwrap();

        let mut cursor = Cursor::new(&buf);
        assert!(Trailer::read(&mut cursor).is_err());
    }

    const TRAILER_SIZE_U64: u64 = TRAILER_SIZE as u64;

    #[test]
    fn test_validate_accepts_well_formed() {
        let trailer = Trailer {
            erts_offset: 0,
            app_offset: 100,
            meta_offset: 200,
        };
        trailer.validate(200 + TRAILER_SIZE_U64).unwrap();
    }

    #[test]
    fn test_validate_rejects_app_before_erts() {
        let trailer = Trailer {
            erts_offset: 100,
            app_offset: 50,
            meta_offset: 200,
        };
        assert!(trailer.validate(200 + TRAILER_SIZE_U64).is_err());
    }

    #[test]
    fn test_validate_rejects_meta_before_app() {
        let trailer = Trailer {
            erts_offset: 0,
            app_offset: 200,
            meta_offset: 100,
        };
        assert!(trailer.validate(200 + TRAILER_SIZE_U64).is_err());
    }

    #[test]
    fn test_validate_rejects_zero_length_erts() {
        let trailer = Trailer {
            erts_offset: 100,
            app_offset: 100,
            meta_offset: 200,
        };
        assert!(trailer.validate(200 + TRAILER_SIZE_U64).is_err());
    }

    #[test]
    fn test_validate_rejects_zero_length_app() {
        let trailer = Trailer {
            erts_offset: 0,
            app_offset: 100,
            meta_offset: 100,
        };
        assert!(trailer.validate(200 + TRAILER_SIZE_U64).is_err());
    }

    #[test]
    fn test_validate_rejects_meta_past_end() {
        let trailer = Trailer {
            erts_offset: 0,
            app_offset: 100,
            meta_offset: 200,
        };
        assert!(trailer.validate(200).is_err());
    }

    #[test]
    fn test_validate_rejects_file_shorter_than_trailer() {
        let trailer = Trailer {
            erts_offset: 0,
            app_offset: 100,
            meta_offset: 200,
        };
        assert!(trailer.validate(TRAILER_SIZE_U64 - 1).is_err());
    }

    #[test]
    fn test_validate_rejects_offset_overflow() {
        let trailer = Trailer {
            erts_offset: 0,
            app_offset: 100,
            meta_offset: u64::MAX,
        };
        assert!(trailer.validate(u64::MAX).is_err());
    }

    fn sample_metadata() -> Metadata {
        Metadata {
            name: "my_app".into(),
            version: "1.0.0".into(),
            entry_module: "my_app@cli".into(),
            erts_version: "15.0".into(),
            erts_hash: "abc123".into(),
            app_hash: "def456".into(),
            boot_path: "releases/28/no_dot_erlang".into(),
        }
    }

    #[test]
    fn test_metadata_validate_rejects_bad_entry_module() {
        let mut m = sample_metadata();
        m.entry_module = "foo','bar".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_metadata_validate_rejects_path_traversal() {
        let mut m = sample_metadata();
        m.boot_path = "releases/../../etc/passwd".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_metadata_bincode_roundtrip() {
        let m = sample_metadata();
        let bytes = bincode::encode_to_vec(&m, bincode::config::standard()).unwrap();
        let (decoded, _): (Metadata, _) =
            bincode::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
        assert_eq!(decoded.name, m.name);
        assert_eq!(decoded.version, m.version);
        assert_eq!(decoded.entry_module, m.entry_module);
        assert_eq!(decoded.erts_version, m.erts_version);
        assert_eq!(decoded.erts_hash, m.erts_hash);
        assert_eq!(decoded.app_hash, m.app_hash);
        assert_eq!(decoded.boot_path, m.boot_path);
    }

    #[test]
    fn test_trailer_metadata_combined_read() {
        let m = sample_metadata();
        let meta_bytes = bincode::encode_to_vec(&m, bincode::config::standard()).unwrap();

        let erts = b"ERTS_DATA";
        let app = b"APP_DATA";
        let erts_offset = 0u64;
        let app_offset = erts_offset + u64::try_from(erts.len()).unwrap();
        let meta_offset = app_offset + u64::try_from(app.len()).unwrap();

        let mut buf = Vec::new();
        buf.extend_from_slice(erts);
        buf.extend_from_slice(app);
        buf.extend_from_slice(&meta_bytes);
        let trailer = Trailer {
            erts_offset,
            app_offset,
            meta_offset,
        };
        write_trailer_bytes(&trailer, &mut buf);

        let file_len = u64::try_from(buf.len()).unwrap();
        let mut cursor = Cursor::new(&buf);
        let read_trailer = Trailer::read(&mut cursor).unwrap();
        read_trailer.validate(file_len).unwrap();

        let meta_end = file_len - TRAILER_SIZE_U64;
        let meta_start = usize::try_from(read_trailer.meta_offset).unwrap();
        let meta_len = usize::try_from(meta_end - read_trailer.meta_offset).unwrap();
        let meta_slice = &buf[meta_start..meta_start + meta_len];
        let (decoded, _): (Metadata, _) =
            bincode::decode_from_slice(meta_slice, bincode::config::standard()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.name, m.name);
        assert_eq!(decoded.boot_path, m.boot_path);
        assert_eq!(decoded.entry_module, m.entry_module);
        assert_eq!(decoded.erts_hash, m.erts_hash);
    }
}
