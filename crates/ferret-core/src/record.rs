//! MFT FILE record parsing.
//!
//! Every file and directory on an NTFS volume owns at least one 1 KB record in
//! `$MFT`. A record is a header followed by a chain of attributes; the ones
//! Ferret cares about are `$FILE_NAME` (0x30), which holds the name and the
//! parent directory, and `$DATA` (0x80), which holds the size.
//!
//! Records are stored with "fixups": the last two bytes of each sector are
//! replaced by a check value and the real bytes are parked in an array in the
//! header. [`apply_fixups`] puts them back — skip it and every 512th byte pair
//! of the record is wrong.

use crate::bytes::{u16le, u32le, u64le, utf16le};

pub const SIGNATURE: &[u8; 4] = b"FILE";

/// Record header flag: the record describes a live file (not a deleted one).
pub const FLAG_IN_USE: u16 = 0x0001;
/// Record header flag: the record describes a directory.
pub const FLAG_DIRECTORY: u16 = 0x0002;

pub const ATTR_STANDARD_INFO: u32 = 0x10;
/// Present when a file's attributes spilled over into extension records.
pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
/// A directory's B-tree, once it outgrows the record. Occupies real clusters.
pub const ATTR_INDEX_ALLOCATION: u32 = 0xA0;
const ATTR_END: u32 = 0xFFFF_FFFF;

/// DOS attribute bits stored in `$STANDARD_INFORMATION`.
const DOS_READONLY: u32 = 0x0001;
const DOS_HIDDEN: u32 = 0x0002;
const DOS_SYSTEM: u32 = 0x0004;
const DOS_SPARSE: u32 = 0x0200;
const DOS_REPARSE: u32 = 0x0400;
const DOS_COMPRESSED: u32 = 0x0800;
const DOS_OFFLINE: u32 = 0x1000;
const DOS_RECALL_ON_OPEN: u32 = 0x0004_0000;
const DOS_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// Attribute header flags.
const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
const ATTR_FLAG_SPARSE: u16 = 0x8000;

mod hdr {
    pub const USA_OFFSET: usize = 0x04;
    pub const USA_COUNT: usize = 0x06;
    pub const FLAGS: usize = 0x16;
    pub const FIRST_ATTR: usize = 0x14;
    pub const USED_SIZE: usize = 0x18;
    pub const BASE_RECORD: usize = 0x20;
}

/// Reverse the fixup encoding in place.
///
/// Returns `false` when the record is not a FILE record or its fixup array is
/// inconsistent, which is the signal to skip it.
pub fn apply_fixups(record: &mut [u8], bytes_per_sector: usize) -> bool {
    if record.len() < 0x30 || &record[0..4] != SIGNATURE {
        return false;
    }

    let usa_offset = u16le(record, hdr::USA_OFFSET) as usize;
    let usa_count = u16le(record, hdr::USA_COUNT) as usize;

    // The array is one check value plus one saved pair per sector.
    if usa_count == 0 || usa_offset + usa_count * 2 > record.len() {
        return false;
    }
    let sectors = usa_count - 1;
    if sectors == 0 || sectors * bytes_per_sector > record.len() {
        return false;
    }

    let check = u16le(record, usa_offset);

    for sector in 0..sectors {
        let tail = (sector + 1) * bytes_per_sector - 2;
        // Each sector must currently end with the check value; if it does not,
        // the record was torn mid-write and its contents cannot be trusted.
        if u16le(record, tail) != check {
            return false;
        }
        let saved = u16le(record, usa_offset + 2 + sector * 2);
        record[tail] = (saved & 0xFF) as u8;
        record[tail + 1] = (saved >> 8) as u8;
    }

    true
}

/// Header fields of a FILE record, after fixups have been applied.
#[derive(Debug, Clone, Copy)]
pub struct RecordHeader {
    pub flags: u16,
    pub first_attribute: usize,
    pub used_size: u32,
    /// For an extension record, the record number of the file it belongs to;
    /// zero for a base record.
    pub base_record: u64,
}

impl RecordHeader {
    pub fn in_use(&self) -> bool {
        self.flags & FLAG_IN_USE != 0
    }
    pub fn is_directory(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }
    pub fn is_extension(&self) -> bool {
        self.base_record != 0
    }
}

pub fn parse_header(record: &[u8]) -> Option<RecordHeader> {
    if record.len() < 0x30 || &record[0..4] != SIGNATURE {
        return None;
    }
    let first_attribute = u16le(record, hdr::FIRST_ATTR) as usize;
    let used_size = u32le(record, hdr::USED_SIZE);
    if first_attribute >= record.len() {
        return None;
    }
    Some(RecordHeader {
        flags: u16le(record, hdr::FLAGS),
        first_attribute,
        used_size,
        // The top 16 bits are a sequence number, not part of the reference.
        base_record: u64le(record, hdr::BASE_RECORD) & 0x0000_FFFF_FFFF_FFFF,
    })
}

/// One attribute in a record's attribute chain.
pub struct Attribute<'a> {
    pub kind: u32,
    pub non_resident: bool,
    pub name_len: u8,
    /// The whole attribute, header included.
    pub raw: &'a [u8],
}

impl<'a> Attribute<'a> {
    /// Content of a resident attribute, or `None` if it is non-resident.
    pub fn resident_value(&self) -> Option<&'a [u8]> {
        if self.non_resident {
            return None;
        }
        let length = u32le(self.raw, 0x10) as usize;
        let offset = u16le(self.raw, 0x14) as usize;
        self.raw.get(offset..offset + length)
    }

    /// Run list bytes of a non-resident attribute.
    pub fn run_list(&self) -> Option<&'a [u8]> {
        if !self.non_resident {
            return None;
        }
        let offset = u16le(self.raw, 0x20) as usize;
        self.raw.get(offset..)
    }

    /// Real (not allocated) content size of a non-resident attribute.
    pub fn non_resident_size(&self) -> Option<u64> {
        if !self.non_resident {
            return None;
        }
        Some(u64le(self.raw, 0x30))
    }

    /// Attribute header flags (compressed, encrypted, sparse).
    pub fn flags(&self) -> u16 {
        u16le(self.raw, 0x0C)
    }

    /// First virtual cluster this piece of a non-resident attribute covers.
    ///
    /// A large, fragmented attribute is split across several records, each
    /// holding a slice of the run list. Only the piece starting at VCN 0
    /// carries meaningful size fields; the others' are zero or stale.
    pub fn lowest_vcn(&self) -> u64 {
        if !self.non_resident {
            return 0;
        }
        u64le(self.raw, 0x10)
    }

    /// Whether this piece carries the attribute's sizes (see [`Self::lowest_vcn`]).
    pub fn is_first_piece(&self) -> bool {
        self.lowest_vcn() == 0
    }

    /// Clusters this piece of the attribute occupies on the volume.
    ///
    /// A resident attribute lives inside its MFT record and takes none. For a
    /// non-resident one, the run list is the ground truth: holes left by
    /// compression or sparseness are not storage (see
    /// [`crate::runs::stored_clusters`]). Unlike the size fields, every piece
    /// of a split attribute carries its own runs, so pieces add up.
    pub fn stored_clusters(&self) -> u64 {
        match self.run_list() {
            Some(list) => crate::runs::stored_clusters(list),
            None => 0,
        }
    }

    /// Whether the attribute's content is compressed or sparse.
    pub fn is_packed(&self) -> bool {
        self.flags() & (ATTR_FLAG_COMPRESSED | ATTR_FLAG_SPARSE) != 0
    }

    /// Logical size: what a directory listing reports.
    pub fn content_size(&self) -> u64 {
        match self.non_resident_size() {
            Some(size) => size,
            None => self.resident_value().map(|v| v.len() as u64).unwrap_or(0),
        }
    }
}

/// Walk the attribute chain of a record.
///
/// The iterator stops at the end marker, at the used-size boundary, or at the
/// first attribute whose declared length is impossible — a corrupt length must
/// not become an infinite loop.
pub fn attributes<'a>(record: &'a [u8], header: &RecordHeader) -> Attributes<'a> {
    let limit = (header.used_size as usize).min(record.len());
    Attributes {
        record,
        pos: header.first_attribute,
        limit,
    }
}

pub struct Attributes<'a> {
    record: &'a [u8],
    pos: usize,
    limit: usize,
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Attribute<'a>;

    fn next(&mut self) -> Option<Attribute<'a>> {
        if self.pos + 8 > self.limit {
            return None;
        }
        let kind = u32le(self.record, self.pos);
        if kind == ATTR_END {
            return None;
        }
        let length = u32le(self.record, self.pos + 4) as usize;
        // A zero or oversized length would loop forever / read out of bounds.
        if length < 0x10 || self.pos + length > self.limit {
            return None;
        }

        let raw = &self.record[self.pos..self.pos + length];
        self.pos += length;

        Some(Attribute {
            kind,
            non_resident: raw[0x08] != 0,
            name_len: raw[0x09],
            raw,
        })
    }
}

/// Which naming scheme a `$FILE_NAME` attribute uses.
///
/// A file usually has two of these: its real long name and an 8.3 `Dos` alias.
/// Indexing the alias would show `PROGRA~1` in results, so it is filtered out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Posix,
    Win32,
    Dos,
    Win32AndDos,
    Unknown(u8),
}

impl Namespace {
    fn from(value: u8) -> Namespace {
        match value {
            0 => Namespace::Posix,
            1 => Namespace::Win32,
            2 => Namespace::Dos,
            3 => Namespace::Win32AndDos,
            other => Namespace::Unknown(other),
        }
    }

    /// Whether this is a name a person would recognise.
    pub fn is_preferred(&self) -> bool {
        matches!(
            self,
            Namespace::Win32 | Namespace::Win32AndDos | Namespace::Posix
        )
    }
}

/// The parts of `$FILE_NAME` that Ferret indexes.
#[derive(Debug, Clone)]
pub struct FileName {
    /// MFT record number of the containing directory.
    pub parent: u64,
    pub name: String,
    pub namespace: Namespace,
    /// Size as recorded in the directory entry; only a hint, `$DATA` wins.
    pub real_size: u64,
    /// Allocated size as recorded in the directory entry; likewise a hint.
    pub allocated_size: u64,
}

/// The parts of `$STANDARD_INFORMATION` that Ferret indexes.
///
/// `$FILE_NAME` carries timestamps too, but Windows does not keep those in step
/// with reality — the authoritative "date modified" a user recognises lives
/// here.
#[derive(Debug, Clone, Copy)]
pub struct StandardInfo {
    pub created: u64,
    pub modified: u64,
    pub dos_attributes: u32,
}

impl StandardInfo {
    /// Translate the DOS attribute bits into [`crate::mft`] entry flags.
    pub fn flags(&self) -> u16 {
        let mut flags = 0u16;
        if self.dos_attributes & DOS_HIDDEN != 0 {
            flags |= crate::mft::IS_HIDDEN;
        }
        if self.dos_attributes & DOS_SYSTEM != 0 {
            flags |= crate::mft::IS_SYSTEM;
        }
        if self.dos_attributes & DOS_READONLY != 0 {
            flags |= crate::mft::IS_READONLY;
        }
        if self.dos_attributes & DOS_COMPRESSED != 0 {
            flags |= crate::mft::IS_COMPRESSED;
        }
        if self.dos_attributes & DOS_SPARSE != 0 {
            flags |= crate::mft::IS_SPARSE;
        }
        if self.dos_attributes & DOS_REPARSE != 0 {
            flags |= crate::mft::IS_REPARSE;
        }
        // OneDrive and friends: the name is here, the bytes are in the cloud.
        if self.dos_attributes & (DOS_OFFLINE | DOS_RECALL_ON_OPEN | DOS_RECALL_ON_DATA_ACCESS) != 0
        {
            flags |= crate::mft::IS_CLOUD;
        }
        flags
    }
}

pub fn parse_standard_info(value: &[u8]) -> Option<StandardInfo> {
    if value.len() < 0x24 {
        return None;
    }
    Some(StandardInfo {
        created: u64le(value, 0x00),
        modified: u64le(value, 0x08),
        dos_attributes: u32le(value, 0x20),
    })
}

pub fn parse_file_name(value: &[u8]) -> Option<FileName> {
    if value.len() < 0x42 {
        return None;
    }
    let name_chars = value[0x40] as usize;
    let name = utf16le(value, 0x42, name_chars)?;

    Some(FileName {
        // The top 16 bits are a sequence number, not part of the reference.
        parent: u64le(value, 0x00) & 0x0000_FFFF_FFFF_FFFF,
        name,
        namespace: Namespace::from(value[0x41]),
        real_size: u64le(value, 0x30),
        allocated_size: u64le(value, 0x28),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal record with a valid fixup array.
    fn record_with_fixups(sectors: usize, sector_size: usize) -> Vec<u8> {
        let mut rec = vec![0u8; sectors * sector_size];
        rec[0..4].copy_from_slice(SIGNATURE);
        let usa_offset = 0x30usize;
        rec[hdr::USA_OFFSET] = usa_offset as u8;
        rec[hdr::USA_COUNT] = (sectors + 1) as u8;

        // Check value 0xBEEF at the array head and at the tail of each sector.
        rec[usa_offset] = 0xEF;
        rec[usa_offset + 1] = 0xBE;
        for s in 0..sectors {
            let tail = (s + 1) * sector_size - 2;
            rec[tail] = 0xEF;
            rec[tail + 1] = 0xBE;
            // The real bytes that belong there: 0xAA00 + sector index.
            rec[usa_offset + 2 + s * 2] = s as u8;
            rec[usa_offset + 3 + s * 2] = 0xAA;
        }
        rec
    }

    #[test]
    fn fixups_restore_the_real_sector_tails() {
        let mut rec = record_with_fixups(2, 512);
        assert!(apply_fixups(&mut rec, 512));
        assert_eq!(u16le(&rec, 512 - 2), 0xAA00);
        assert_eq!(u16le(&rec, 1024 - 2), 0xAA01);
    }

    #[test]
    fn a_torn_record_is_rejected() {
        let mut rec = record_with_fixups(2, 512);
        // Corrupt the second sector's check value.
        rec[1024 - 1] = 0x00;
        assert!(!apply_fixups(&mut rec, 512));
    }

    #[test]
    fn non_file_records_are_rejected() {
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(b"BAAD");
        assert!(!apply_fixups(&mut rec, 512));
        assert!(parse_header(&rec).is_none());
    }

    #[test]
    fn parses_a_file_name_attribute() {
        let mut value = vec![0u8; 0x42];
        value[0x00] = 0x05; // parent = record 5 (the root directory)
        value[0x30] = 0x10; // real size = 16
        value[0x40] = 6; // six characters
        value[0x41] = 1; // Win32 namespace
        for ch in "Ferret".encode_utf16() {
            value.extend_from_slice(&ch.to_le_bytes());
        }

        let parsed = parse_file_name(&value).expect("should parse");
        assert_eq!(parsed.parent, 5);
        assert_eq!(parsed.name, "Ferret");
        assert_eq!(parsed.namespace, Namespace::Win32);
        assert_eq!(parsed.real_size, 16);
        assert!(parsed.namespace.is_preferred());
    }

    #[test]
    fn parses_standard_information() {
        let mut value = vec![0u8; 0x30];
        value[0x08..0x10].copy_from_slice(&126_227_808_000_000_000u64.to_le_bytes());
        value[0x20..0x24].copy_from_slice(&(DOS_HIDDEN | DOS_SYSTEM).to_le_bytes());

        let info = parse_standard_info(&value).expect("should parse");
        assert_eq!(info.modified, 126_227_808_000_000_000);
        assert!(info.flags() & crate::mft::IS_HIDDEN != 0);
        assert!(info.flags() & crate::mft::IS_SYSTEM != 0);
        assert!(info.flags() & crate::mft::IS_READONLY == 0);
    }

    #[test]
    fn a_truncated_standard_information_is_rejected() {
        assert!(parse_standard_info(&[0u8; 8]).is_none());
    }

    #[test]
    fn dos_alias_names_are_not_preferred() {
        assert!(!Namespace::from(2).is_preferred());
        assert!(Namespace::from(3).is_preferred());
    }

    /// A non-resident attribute header with the given sizes, flags and runs.
    fn non_resident(
        flags: u16,
        lowest_vcn: u64,
        allocated: u64,
        real: u64,
        runs: &[u8],
    ) -> Vec<u8> {
        let mut raw = vec![0u8; 0x48];
        raw[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        raw[0x04..0x08].copy_from_slice(&((0x48 + runs.len()) as u32).to_le_bytes());
        raw[0x08] = 1;
        raw[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        raw[0x10..0x18].copy_from_slice(&lowest_vcn.to_le_bytes());
        raw[0x20..0x22].copy_from_slice(&0x48u16.to_le_bytes());
        raw[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
        raw[0x30..0x38].copy_from_slice(&real.to_le_bytes());
        raw.extend_from_slice(runs);
        raw
    }

    fn attribute(raw: &[u8]) -> Attribute<'_> {
        Attribute {
            kind: ATTR_DATA,
            non_resident: raw[0x08] != 0,
            name_len: raw[0x09],
            raw,
        }
    }

    #[test]
    fn a_plain_file_occupies_its_runs() {
        // Two clusters at LCN 0x60.
        let raw = non_resident(0, 0, 8192, 5000, &[0x11, 0x02, 0x60, 0x00]);
        let attr = attribute(&raw);
        assert_eq!(attr.content_size(), 5000);
        assert_eq!(attr.stored_clusters(), 2);
        assert!(attr.is_first_piece());
        assert!(!attr.is_packed());
    }

    #[test]
    fn a_compressed_file_occupies_only_what_it_stores() {
        // One 16-cluster compression unit squeezed into 3 clusters + 13 hole.
        let raw = non_resident(
            ATTR_FLAG_COMPRESSED,
            0,
            16 * 4096,
            16 * 4096,
            &[0x11, 0x03, 0x60, 0x01, 0x0D, 0x00],
        );
        let attr = attribute(&raw);
        assert!(attr.is_packed());
        assert_eq!(attr.stored_clusters(), 3);
    }

    #[test]
    fn a_later_piece_is_recognised() {
        let raw = non_resident(0, 1234, 0, 0, &[0x11, 0x04, 0x60, 0x00]);
        let attr = attribute(&raw);
        assert!(!attr.is_first_piece());
        // Its runs still count: pieces add up.
        assert_eq!(attr.stored_clusters(), 4);
    }

    #[test]
    fn a_resident_attribute_takes_no_clusters() {
        let mut raw = vec![0u8; 0x20];
        raw[0x10..0x14].copy_from_slice(&5u32.to_le_bytes());
        raw[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
        let attr = attribute(&raw);
        assert_eq!(attr.content_size(), 5);
        assert_eq!(attr.stored_clusters(), 0);
    }

    #[test]
    fn cloud_placeholders_are_flagged() {
        let mut value = vec![0u8; 0x30];
        value[0x20..0x24].copy_from_slice(&(DOS_RECALL_ON_DATA_ACCESS | DOS_REPARSE).to_le_bytes());
        let flags = parse_standard_info(&value).unwrap().flags();
        assert!(flags & crate::mft::IS_CLOUD != 0);
        assert!(flags & crate::mft::IS_REPARSE != 0);
    }

    #[test]
    fn extension_records_point_at_their_base() {
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(SIGNATURE);
        rec[hdr::FIRST_ATTR] = 0x38;
        // Record 1234, sequence number 7 in the top 16 bits.
        let reference = 1234u64 | (7u64 << 48);
        rec[hdr::BASE_RECORD..hdr::BASE_RECORD + 8].copy_from_slice(&reference.to_le_bytes());
        let header = parse_header(&rec).unwrap();
        assert!(header.is_extension());
        assert_eq!(header.base_record, 1234);
    }

    #[test]
    fn attribute_walk_stops_on_a_corrupt_length() {
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(SIGNATURE);
        rec[hdr::FIRST_ATTR] = 0x38;
        rec[hdr::USED_SIZE] = 0xFF;

        // One well-formed attribute, then one claiming length 0.
        let first = 0x38usize;
        rec[first..first + 4].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        rec[first + 4..first + 8].copy_from_slice(&0x20u32.to_le_bytes());
        let second = first + 0x20;
        rec[second..second + 4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        // length stays 0 -> iterator must stop rather than spin.

        let header = parse_header(&rec).unwrap();
        let kinds: Vec<u32> = attributes(&rec, &header).map(|a| a.kind).collect();
        assert_eq!(kinds, vec![ATTR_FILE_NAME]);
    }
}
