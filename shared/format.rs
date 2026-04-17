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
    use quickcheck_macros::quickcheck;

    use super::*;

    const TRAILER_SIZE_U64: u64 = TRAILER_SIZE as u64;

    #[test]
    fn test_trailer_size() {
        assert_eq!(TRAILER_SIZE, 3 * size_of::<u64>() + TRAILER_MAGIC.len());
    }

    #[quickcheck]
    fn test_trailer_roundtrip(erts_offset: u64, app_offset: u64, meta_offset: u64) -> bool {
        let trailer = Trailer {
            erts_offset,
            app_offset,
            meta_offset,
        };

        let mut buf = Vec::with_capacity(TRAILER_SIZE);
        trailer.write(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf);
        let read_back = Trailer::read(&mut cursor).unwrap();

        read_back.erts_offset == erts_offset
            && read_back.app_offset == app_offset
            && read_back.meta_offset == meta_offset
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

    #[test]
    fn test_validate_cases() {
        let cases: &[(&str, Trailer, u64, Option<&str>)] = &[
            (
                "well-formed",
                Trailer {
                    erts_offset: 0,
                    app_offset: 100,
                    meta_offset: 200,
                },
                200 + TRAILER_SIZE_U64,
                None,
            ),
            (
                "app before erts",
                Trailer {
                    erts_offset: 100,
                    app_offset: 50,
                    meta_offset: 200,
                },
                200 + TRAILER_SIZE_U64,
                Some("trailer: erts_offset must be less than app_offset"),
            ),
            (
                "meta before app",
                Trailer {
                    erts_offset: 0,
                    app_offset: 200,
                    meta_offset: 100,
                },
                200 + TRAILER_SIZE_U64,
                Some("trailer: app_offset must be less than meta_offset"),
            ),
            (
                "zero-length erts",
                Trailer {
                    erts_offset: 100,
                    app_offset: 100,
                    meta_offset: 200,
                },
                200 + TRAILER_SIZE_U64,
                Some("trailer: erts_offset must be less than app_offset"),
            ),
            (
                "zero-length app",
                Trailer {
                    erts_offset: 0,
                    app_offset: 100,
                    meta_offset: 100,
                },
                200 + TRAILER_SIZE_U64,
                Some("trailer: app_offset must be less than meta_offset"),
            ),
            (
                "meta past end",
                Trailer {
                    erts_offset: 0,
                    app_offset: 100,
                    meta_offset: 200,
                },
                200,
                Some("trailer: metadata extends past end of file"),
            ),
            (
                "file shorter than trailer",
                Trailer {
                    erts_offset: 0,
                    app_offset: 100,
                    meta_offset: 200,
                },
                TRAILER_SIZE_U64 - 1,
                Some("binary too small to contain trailer"),
            ),
            (
                "offset overflow",
                Trailer {
                    erts_offset: 0,
                    app_offset: 100,
                    meta_offset: u64::MAX,
                },
                u64::MAX,
                Some("trailer: meta_offset + trailer size overflows"),
            ),
        ];

        for (_label, trailer, file_len, expected_err) in cases {
            let result = trailer.validate(*file_len);
            match expected_err {
                None => assert!(result.is_ok()),
                Some(msg) => assert_eq!(result.unwrap_err().to_string(), *msg),
            }
        }
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
    fn test_metadata_validate_cases() {
        type Mutate = fn(&mut Metadata);
        let cases: &[(&str, Mutate, Option<&str>)] = &[
            ("baseline", |_| {}, None),
            (
                "empty name",
                |m| m.name = String::new(),
                Some("metadata: name is empty"),
            ),
            (
                "name is dotdot",
                |m| m.name = "..".into(),
                Some("metadata: name is '.' or '..'"),
            ),
            (
                "name has slash",
                |m| m.name = "foo/bar".into(),
                Some("metadata: name contains path separator"),
            ),
            (
                "name has backslash",
                |m| m.name = "foo\\bar".into(),
                Some("metadata: name contains path separator"),
            ),
            (
                "empty version",
                |m| m.version = String::new(),
                Some("metadata: version is empty"),
            ),
            (
                "empty erts_version",
                |m| m.erts_version = String::new(),
                Some("metadata: erts_version is empty"),
            ),
            (
                "empty entry_module",
                |m| m.entry_module = String::new(),
                Some("metadata: entry_module is empty"),
            ),
            (
                "bad entry_module chars",
                |m| m.entry_module = "foo','bar".into(),
                Some("metadata: entry_module contains disallowed characters"),
            ),
            (
                "empty boot_path",
                |m| m.boot_path = String::new(),
                Some("metadata: boot_path is empty"),
            ),
            (
                "absolute boot_path",
                |m| m.boot_path = "/etc/passwd".into(),
                Some("metadata: boot_path must be relative"),
            ),
            (
                "boot_path traversal",
                |m| m.boot_path = "releases/../../etc/passwd".into(),
                Some("metadata: boot_path contains '.' or '..' component"),
            ),
        ];

        for (_label, mutate, expected_err) in cases {
            let mut m = sample_metadata();
            mutate(&mut m);
            let result = m.validate();
            match expected_err {
                None => assert!(result.is_ok()),
                Some(msg) => assert_eq!(result.unwrap_err().to_string(), *msg),
            }
        }
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
        trailer.write(&mut buf).unwrap();

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
