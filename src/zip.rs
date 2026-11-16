//! A minimal ZIP reader for dependency jars.
//!
//! A jar is a ZIP file, and the plugin only ever needs two things from one:
//! the central directory (to learn which entries exist) and one entry's bytes
//! (a `.class` file). The reader therefore handles just stored and deflated
//! entries; ZIP64 and encryption are reported as unsupported, which is fine
//! for the library jars this plugin looks at.

use std::io::Read;

/// Where one entry lives in the archive.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    method: u16,
    compressed: usize,
    uncompressed: usize,
    local: usize,
}

/// A read-only view of a ZIP archive held in memory.
pub struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> Zip<'a> {
    /// Parses the central directory. `None` means "not a readable ZIP".
    pub fn open(data: &'a [u8]) -> Option<Self> {
        let eocd = find_eocd(data)?;
        let count = le16(data, eocd + 10)? as usize;
        let offset = le32(data, eocd + 16)? as usize;
        if count == 0xffff || offset == 0xffff_ffff {
            return None; // ZIP64: library jars are smaller than this.
        }
        let mut entries = Vec::with_capacity(count);
        let mut at = offset;
        for _ in 0..count {
            if le32(data, at)? != 0x0201_4b50 {
                return None;
            }
            let method = le16(data, at + 10)?;
            let compressed = le32(data, at + 20)? as usize;
            let uncompressed = le32(data, at + 24)? as usize;
            let name_len = le16(data, at + 28)? as usize;
            let extra_len = le16(data, at + 30)? as usize;
            let comment_len = le16(data, at + 32)? as usize;
            let local = le32(data, at + 42)? as usize;
            let name = std::str::from_utf8(data.get(at + 46..at + 46 + name_len)?).ok()?.to_owned();
            entries.push(Entry { name, method, compressed, uncompressed, local });
            at = at.checked_add(46 + name_len + extra_len + comment_len)?;
        }
        Some(Zip { data, entries })
    }

    /// The entry whose path ends with `suffix` (`/Foo.class`), preferring the
    /// shortest match so `com/acme/Foo.class` wins over `x/Foo.class`.
    pub fn find(&self, suffix: &str) -> Option<&str> {
        self.entries
            .iter()
            .filter(|entry| entry.name.ends_with(suffix))
            .min_by_key(|entry| entry.name.len())
            .map(|entry| entry.name.as_str())
    }

    /// Every entry name, in central-directory order.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    /// The uncompressed bytes of `name`, at most `limit` bytes.
    pub fn read(&self, name: &str, limit: usize) -> Option<Vec<u8>> {
        let entry = self.entries.iter().find(|entry| entry.name == name)?;
        if entry.uncompressed > limit {
            return None;
        }
        let local = entry.local;
        if le32(self.data, local)? != 0x0403_4b50 {
            return None;
        }
        let name_len = le16(self.data, local + 26)? as usize;
        let extra_len = le16(self.data, local + 28)? as usize;
        let start = local.checked_add(30 + name_len + extra_len)?;
        let raw = self.data.get(start..start.checked_add(entry.compressed)?)?;
        match entry.method {
            0 => Some(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(entry.uncompressed);
                flate2::read::DeflateDecoder::new(raw).take(limit as u64).read_to_end(&mut out).ok()?;
                Some(out)
            }
            _ => None,
        }
    }
}

/// The end-of-central-directory record: its offset, scanning back over a
/// possible archive comment.
fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let last = data.len() - 22;
    let first = last.saturating_sub(u16::MAX as usize);
    (first..=last).rev().find(|&at| le32(data, at) == Some(0x0605_4b50))
}

fn le16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored-only ZIP: two entries, no compression, no extra fields.
    fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, body) in files {
            let offset = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0u16.to_le_bytes()); // date
            out.extend_from_slice(&0u32.to_le_bytes()); // crc: not checked
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(body);

            directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes()); // flags
            directory.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            directory.extend_from_slice(&0u16.to_le_bytes()); // time
            directory.extend_from_slice(&0u16.to_le_bytes()); // date
            directory.extend_from_slice(&0u32.to_le_bytes()); // crc: not checked
            directory.extend_from_slice(&(body.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(body.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes()); // disk number
            directory.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            directory.extend_from_slice(&0u32.to_le_bytes()); // external attributes
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn reads_stored_entries_and_matches_by_suffix() {
        let bytes = stored_zip(&[("com/acme/User.class", b"first"), ("com/acme/ApiEnvelope.class", b"second")]);
        let zip = Zip::open(&bytes).expect("a zip");
        let entry = zip.find("/ApiEnvelope.class").expect("the class");
        assert_eq!(entry, "com/acme/ApiEnvelope.class");
        assert_eq!(zip.read(entry, 1024).as_deref(), Some(&b"second"[..]));
        assert_eq!(zip.names().count(), 2);
    }

    #[test]
    fn a_broken_archive_is_rejected() {
        assert!(Zip::open(b"not a zip at all").is_none());
        let bytes = stored_zip(&[("a.txt", b"x")]);
        let zip = Zip::open(&bytes).expect("a zip");
        assert!(zip.find("/missing.class").is_none());
        assert!(zip.read("a.txt", 0).is_none(), "the limit is enforced");
    }
}
