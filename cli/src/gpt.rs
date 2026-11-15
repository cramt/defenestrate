//! GPTs as `sfdisk --json` reports them, and the pure half of
//! `flash --into-free-space`: where on a disk Linux already uses the image's
//! partitions go, and pointing the image's BCD at that disk.

use std::fmt;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

/// Partitions start on 1 MiB boundaries, like every partitioning tool has
/// made them for 15 years (and the image's own).
pub const ALIGN: u64 = 2048;
pub const SECTOR: u64 = 512;

const ESP_TYPE: Guid = Guid::parse_const("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
const MSR_TYPE: Guid = Guid::parse_const("E3C9E316-0B5C-4DB8-817D-F92DF00215AE");
const BASIC_DATA_TYPE: Guid = Guid::parse_const("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");

/// A GUID, held in the textual (big-endian) byte order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid([u8; 16]);

impl Guid {
    const fn parse_const(s: &str) -> Guid {
        match Guid::parse(s) {
            Some(g) => g,
            None => panic!("bad GUID constant"),
        }
    }

    pub const fn parse(s: &str) -> Option<Guid> {
        let s = s.as_bytes();
        if s.len() != 36 {
            return None;
        }
        let mut out = [0u8; 16];
        let (mut i, mut n) = (0, 0);
        while i < 36 {
            if matches!(i, 8 | 13 | 18 | 23) {
                if s[i] != b'-' {
                    return None;
                }
                i += 1;
                continue;
            }
            let (Some(hi), Some(lo)) = (hex(s[i]), hex(s[i + 1])) else { return None };
            out[n] = hi << 4 | lo;
            n += 1;
            i += 2;
        }
        Some(Guid(out))
    }

    /// How GPT and the BCD store it: the first three fields little-endian.
    pub fn on_disk(&self) -> [u8; 16] {
        let mut b = self.0;
        b[0..4].reverse();
        b[4..6].reverse();
        b[6..8].reverse();
        b
    }
}

const fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// A GPT on a 512-byte-sector disk; nothing else parses into one.
#[derive(Debug)]
pub struct Table {
    pub id: Guid,
    /// First and last usable LBA.
    pub first: u64,
    pub last: u64,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug)]
pub struct Part {
    /// The partition number the kernel names it by.
    pub number: u32,
    pub start: u64,
    pub size: u64,
    pub kind: Guid,
    pub uuid: Guid,
    pub name: String,
    /// sfdisk's attribute string, e.g. "GUID:63" (no drive letter).
    pub attrs: Option<String>,
}

impl Part {
    pub fn end(&self) -> u64 {
        self.start + self.size
    }
}

/// Parses `sfdisk --json`. Refuses MBR and non-512-byte sectors: the image's
/// NTFS and FAT are formatted for 512-byte sectors and wouldn't mount on a
/// 4Kn disk.
pub fn parse(json: &str) -> Result<Table> {
    #[derive(Deserialize)]
    struct Dump {
        partitiontable: Raw,
    }
    #[derive(Deserialize)]
    struct Raw {
        label: String,
        id: String,
        unit: String,
        firstlba: Option<u64>,
        lastlba: Option<u64>,
        sectorsize: Option<u64>,
        #[serde(default)]
        partitions: Vec<RawPart>,
    }
    #[derive(Deserialize)]
    struct RawPart {
        node: String,
        start: u64,
        size: u64,
        #[serde(rename = "type")]
        kind: String,
        uuid: String,
        #[serde(default)]
        name: String,
        attrs: Option<String>,
    }
    let raw = serde_json::from_str::<Dump>(json).context("parsing sfdisk --json")?.partitiontable;
    if raw.label != "gpt" {
        bail!("the disk has a {} partition table, not GPT; Windows boots from a GPT disk in UEFI mode only", raw.label);
    }
    ensure!(raw.unit == "sectors", "sfdisk reported unit {}", raw.unit);
    let sector = raw.sectorsize.unwrap_or(SECTOR);
    if sector != SECTOR {
        bail!("the disk has {sector}-byte sectors; the image's filesystems are made for {SECTOR}-byte ones");
    }
    let guid = |s: &str| Guid::parse(s).with_context(|| format!("sfdisk gave a malformed GUID {s}"));
    let parts = raw
        .partitions
        .into_iter()
        .map(|p| {
            let digits = p.node.len() - p.node.trim_end_matches(|c: char| c.is_ascii_digit()).len();
            let number = p.node[p.node.len() - digits..].parse().with_context(|| format!("no partition number in {}", p.node))?;
            Ok(Part { number, start: p.start, size: p.size, kind: guid(&p.kind)?, uuid: guid(&p.uuid)?, name: p.name, attrs: p.attrs })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Table {
        id: guid(&raw.id)?,
        first: raw.firstlba.context("sfdisk reported no firstlba")?,
        last: raw.lastlba.context("sfdisk reported no lastlba")?,
        parts,
    })
}

/// The partitions of a `build image` disk, checked to be what `flash`
/// expects: ESP, MSR, Windows, in that order and nothing else.
#[derive(Debug)]
pub struct Image {
    pub disk: Guid,
    pub esp: Part,
    pub msr: Part,
    pub windows: Part,
}

impl Image {
    pub fn from_table(t: Table) -> Result<Image> {
        let kinds: Vec<Guid> = t.parts.iter().map(|p| p.kind).collect();
        if kinds != [ESP_TYPE, MSR_TYPE, BASIC_DATA_TYPE] {
            bail!("the image isn't laid out as ESP, MSR, Windows; was it made by `build image`?");
        }
        let mut parts = t.parts.into_iter();
        let (esp, msr, windows) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        Ok(Image { disk: t.id, esp, msr, windows })
    }

    fn parts(&self) -> [&Part; 3] {
        [&self.esp, &self.msr, &self.windows]
    }
}

/// Unpartitioned, aligned sectors: [start, end).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Free {
    pub start: u64,
    pub end: u64,
}

impl Free {
    pub fn size(&self) -> u64 {
        self.end - self.start
    }
}

fn align_up(n: u64) -> u64 {
    n.div_ceil(ALIGN) * ALIGN
}

fn align_down(n: u64) -> u64 {
    n / ALIGN * ALIGN
}

/// Every gap of at least one aligned MiB between the table's partitions.
pub fn free(t: &Table) -> Vec<Free> {
    let mut parts: Vec<&Part> = t.parts.iter().collect();
    parts.sort_by_key(|p| p.start);
    let mut out = Vec::new();
    let mut cursor = align_up(t.first);
    let mut push = |start: u64, end: u64| {
        if end > start {
            out.push(Free { start, end });
        }
    };
    for p in parts {
        push(cursor, align_down(p.start));
        cursor = cursor.max(align_up(p.end()));
    }
    push(cursor, align_down(t.last + 1));
    out
}

/// Where one of the image's partitions goes.
#[derive(Debug)]
pub struct Placement {
    pub source: Part,
    pub start: u64,
    pub size: u64,
}

impl Placement {
    pub fn end(&self) -> u64 {
        self.start + self.size
    }
}

/// The image's three partitions, back to back at the start of one free
/// region, Windows taking the rest of it.
#[derive(Debug)]
pub struct Plan {
    pub region: Free,
    pub esp: Placement,
    pub msr: Placement,
    pub windows: Placement,
}

impl Plan {
    pub fn placements(&self) -> [&Placement; 3] {
        [&self.esp, &self.msr, &self.windows]
    }
}

/// Picks the one free region the image fits in. Refuses rather than guess:
/// no region fits, or more than one does (which of them the user freed for
/// Windows isn't ours to pick), or the image's partition GUIDs are already
/// on the disk (an earlier flash of the same image).
pub fn plan(disk: &Table, image: Image) -> Result<Plan> {
    for p in image.parts() {
        if let Some(clash) = disk.parts.iter().find(|d| d.uuid == p.uuid) {
            bail!(
                "partition {} already has the image's partition GUID {}; this image was flashed onto the disk before. Delete those partitions first",
                clash.number,
                p.uuid
            );
        }
    }
    let need: u64 = image.parts().iter().map(|p| align_up(p.size)).sum();
    let regions = free(disk);
    let fits: Vec<Free> = regions.iter().copied().filter(|r| r.size() >= need).collect();
    let region = match fits.as_slice() {
        [one] => *one,
        [] => {
            let largest = regions.iter().map(Free::size).max().unwrap_or(0);
            bail!(
                "no free region is big enough: the image needs {} GiB of contiguous unpartitioned space, the largest has {} GiB. Shrink a Linux filesystem and its partition first",
                gib(need),
                gib(largest)
            );
        }
        many => bail!(
            "{} free regions are big enough ({}); leave only the one meant for Windows",
            many.len(),
            many.iter().map(|r| format!("{} GiB at sector {}", gib(r.size()), r.start)).collect::<Vec<_>>().join(", ")
        ),
    };
    let esp = Placement { start: region.start, size: image.esp.size, source: image.esp };
    let msr = Placement { start: align_up(esp.end()), size: image.msr.size, source: image.msr };
    let start = align_up(msr.end());
    let windows = Placement { start, size: region.end - start, source: image.windows };
    Ok(Plan { region, esp, msr, windows })
}

pub fn gib(sectors: u64) -> u64 {
    (sectors * SECTOR) >> 30
}

/// Points a BCD hive's device elements at a different disk. Each one names
/// a partition by GPT partition GUID *and* disk GUID; the partitions keep
/// the image's GUIDs, but a disk Linux is already on keeps its own. A plain
/// byte swap: the GUID is random, so it doesn't turn up by chance, and only
/// the base block (which never holds it) is checksummed. Refuses a hive
/// with unreplayed log entries, which Windows would apply over the swap.
pub fn retarget_bcd(hive: &mut [u8], from: Guid, to: Guid) -> Result<usize> {
    const BASE_BLOCK: usize = 4096;
    ensure!(hive.len() > BASE_BLOCK && &hive[..4] == b"regf", "the BCD isn't a registry hive");
    let seq = |o: usize| u32::from_le_bytes(hive[o..o + 4].try_into().unwrap());
    ensure!(seq(4) == seq(8), "the BCD hive has unreplayed log entries (sequence {} vs {})", seq(4), seq(8));
    let (from, to) = (from.on_disk(), to.on_disk());
    let mut count = 0;
    let mut i = BASE_BLOCK;
    while i + 16 <= hive.len() {
        if hive[i..i + 16] == from {
            hive[i..i + 16].copy_from_slice(&to);
            count += 1;
            i += 16;
        } else {
            i += 1;
        }
    }
    ensure!(count > 0, "the BCD doesn't name the image's disk; was it made by `build image`?");
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `sfdisk --json` of the image in /pool/dev/defen-wsl/image.
    const IMAGE: &str = r#"{"partitiontable": {"label": "gpt","id": "338B79F5-35F1-4357-B6FD-53F4596C386E","device": "head.raw","unit": "sectors","firstlba": 34,"lastlba": 134217694,"sectorsize": 512,"partitions": [
        {"node": "head.raw1","start": 2048,"size": 614400,"type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B","uuid": "0FDE5DFE-9725-4F5E-9E81-8AE3CCE7DFC9","name": "EFI system partition","attrs": "GUID:63"},
        {"node": "head.raw2","start": 616448,"size": 32768,"type": "E3C9E316-0B5C-4DB8-817D-F92DF00215AE","uuid": "D7E85E36-06AE-4716-A82A-F43E71BBCBAC","name": "Microsoft reserved partition","attrs": "GUID:63"},
        {"node": "head.raw3","start": 649216,"size": 133566464,"type": "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7","uuid": "1D52C887-E063-4FDE-9556-C5730ACE99B7","name": "Basic data partition"}]}}"#;

    /// A 1 TB NVMe with a 1 GiB ESP and a Linux root ending at `root_end`.
    fn linux(root_end: u64, extra: &str) -> Table {
        parse(&format!(
            r#"{{"partitiontable": {{"label": "gpt","id": "11111111-2222-3333-4444-555555555555","device": "/dev/nvme0n1","unit": "sectors","firstlba": 34,"lastlba": 1953525134,"sectorsize": 512,"partitions": [
            {{"node": "/dev/nvme0n1p1","start": 2048,"size": 2097152,"type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B","uuid": "AAAAAAAA-0000-0000-0000-000000000001"}},
            {{"node": "/dev/nvme0n1p2","start": 2099200,"size": {},"type": "0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid": "AAAAAAAA-0000-0000-0000-000000000002"}}{extra}]}}}}"#,
            root_end - 2099200
        ))
        .unwrap()
    }

    fn image() -> Image {
        Image::from_table(parse(IMAGE).unwrap()).unwrap()
    }

    #[test]
    fn guid_round_trips_and_mixes_endianness() {
        let g = Guid::parse("338B79F5-35F1-4357-B6FD-53F4596C386E").unwrap();
        assert_eq!(g.to_string(), "338B79F5-35F1-4357-B6FD-53F4596C386E");
        // As it appears in the image's BCD.
        assert_eq!(g.on_disk(), [0xf5, 0x79, 0x8b, 0x33, 0xf1, 0x35, 0x57, 0x43, 0xb6, 0xfd, 0x53, 0xf4, 0x59, 0x6c, 0x38, 0x6e]);
        assert!(Guid::parse("338B79F5-35F1-4357-B6FD-53F4596C386").is_none());
        assert!(Guid::parse("338B79F5x35F1-4357-B6FD-53F4596C386E").is_none());
    }

    #[test]
    fn plans_windows_into_the_free_tail() {
        let disk = linux(500 << 21, ""); // root ends at 500 GiB
        let plan = plan(&disk, image()).unwrap();
        assert_eq!(plan.region, Free { start: 500 << 21, end: 1953523712 });
        assert_eq!(plan.esp.start, 500 << 21);
        assert_eq!(plan.msr.start, plan.esp.end());
        assert_eq!(plan.windows.start, plan.msr.end());
        assert_eq!(plan.windows.end(), plan.region.end);
        assert!(plan.windows.size > plan.windows.source.size);
        for p in plan.placements() {
            assert_eq!(p.start % ALIGN, 0);
            assert!(disk.parts.iter().all(|d| p.end() <= d.start || p.start >= d.end()));
        }
    }

    #[test]
    fn unaligned_neighbours_are_stepped_around() {
        // Root ends mid-MiB: the region starts at the next boundary.
        let disk = linux((500 << 21) + 7, "");
        assert_eq!(plan(&disk, image()).unwrap().region.start, (500 << 21) + ALIGN);
    }

    #[test]
    fn refuses_too_little_space() {
        let disk = linux(1953525134 - (60 << 21), "");
        let err = plan(&disk, image()).unwrap_err().to_string();
        assert!(err.contains("no free region is big enough"), "{err}");
    }

    #[test]
    fn refuses_two_candidate_regions() {
        // Root ends at 300 GiB, a data partition at 500-600, free after it.
        let extra = r#",{"node": "/dev/nvme0n1p3","start": 1048576000,"size": 209715200,"type": "0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid": "AAAAAAAA-0000-0000-0000-000000000003"}"#;
        let disk = linux(300 << 21, extra);
        let err = plan(&disk, image()).unwrap_err().to_string();
        assert!(err.contains("2 free regions are big enough"), "{err}");
    }

    #[test]
    fn refuses_an_earlier_flash_of_the_image() {
        let extra = r#",{"node": "/dev/nvme0n1p3","start": 1048576000,"size": 614400,"type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B","uuid": "0FDE5DFE-9725-4F5E-9E81-8AE3CCE7DFC9"}"#;
        let err = plan(&linux(300 << 21, extra), image()).unwrap_err().to_string();
        assert!(err.contains("flashed onto the disk before"), "{err}");
    }

    #[test]
    fn refuses_mbr_and_4kn() {
        let mbr = r#"{"partitiontable": {"label": "dos","id": "0x1234","device": "/dev/sda","unit": "sectors","sectorsize": 512,"partitions": []}}"#;
        assert!(parse(mbr).unwrap_err().to_string().contains("not GPT"));
        let k4 = IMAGE.replace("\"sectorsize\": 512", "\"sectorsize\": 4096");
        assert!(parse(&k4).unwrap_err().to_string().contains("4096-byte sectors"));
    }

    #[test]
    fn refuses_an_image_of_another_shape() {
        let two = IMAGE.replace("E3C9E316-0B5C-4DB8-817D-F92DF00215AE", "0FC63DAF-8483-4772-8E79-3D69D8477DE4");
        assert!(Image::from_table(parse(&two).unwrap()).is_err());
    }

    #[test]
    fn retargets_the_bcd() {
        let from = Guid::parse("338B79F5-35F1-4357-B6FD-53F4596C386E").unwrap();
        let to = Guid::parse("11111111-2222-3333-4444-555555555555").unwrap();
        let mut hive = vec![0u8; 8192];
        hive[..4].copy_from_slice(b"regf");
        hive[4] = 21;
        hive[8] = 21;
        hive[5000..5016].copy_from_slice(&from.on_disk());
        hive[6001..6017].copy_from_slice(&from.on_disk());
        assert_eq!(retarget_bcd(&mut hive, from, to).unwrap(), 2);
        assert_eq!(hive[5000..5016], to.on_disk());
        assert_eq!(hive[6001..6017], to.on_disk());
        assert!(retarget_bcd(&mut hive, from, to).is_err());
        hive[8] = 22;
        assert!(retarget_bcd(&mut hive, to, from).unwrap_err().to_string().contains("unreplayed"));
    }
}
