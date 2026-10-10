//! 校验 fMP4 样本结构并续接逐轨解码时间轴.

use std::ops::Range;

use anyhow::{ensure, Context, Result};

pub const VIDEO_TIMESCALE: u32 = 90_000;
pub const AUDIO_TIMESCALE: u32 = 48_000;

/// 提取共享 init 的解码契约, 无关的编码器名称和码率提示不参与兼容性比较.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub id: u32,
    pub timescale: u32,
    pub video: bool,
    description: Vec<u8>,
    // trex 中依次为样本描述索引, 默认时长, 大小和标志.
    defaults: [u32; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Init {
    pub tracks: Vec<Track>,
}

/// 视频使用 90 kHz 刻度, 音频使用 48 kHz 刻度, 两条轨道分别累计以避免浮点漂移.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    pub video: u64,
    pub audio: u64,
}

impl Tail {
    pub fn seconds(self) -> f64 { self.video as f64 / VIDEO_TIMESCALE as f64 }

    fn get(self, video: bool) -> u64 { if video { self.video } else { self.audio } }
}

/// 各轨道的半开时间区间, end 必须等于下一分片对应轨道的 start.
#[derive(Clone, Debug)]
pub struct Fragment {
    pub start: Tail,
    pub end: Tail,
    pub sequence: u32,
}

#[derive(Clone)]
struct Mp4Box {
    kind: [u8; 4],
    start: usize,
    payload: Range<usize>,
}

fn number(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(data.get(offset..offset.checked_add(4).context("MP4 offset overflow")?)
        .context("truncated MP4 number")?.try_into()?))
}

fn wide(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(data.get(offset..offset.checked_add(8).context("MP4 offset overflow")?)
        .context("truncated MP4 timestamp")?.try_into()?))
}

// 所有子盒都受父盒边界约束, 同时处理普通长度, 64 位扩展长度和延伸至父盒末尾的长度.
fn boxes(data: &[u8], range: Range<usize>) -> Result<Vec<Mp4Box>> {
    ensure!(range.end <= data.len(), "MP4 box exceeds its parent");
    let mut result = Vec::new();
    let mut position = range.start;
    while position < range.end {
        ensure!(range.end - position >= 8, "truncated MP4 box header");
        let size = number(data, position)?;
        let (size, header) = match size {
            0 => (range.end - position, 8),
            1 => (usize::try_from(wide(data, position + 8)?)?, 16),
            size => (size as usize, 8),
        };
        ensure!(size >= header && size <= range.end - position, "truncated MP4 box");
        result.push(Mp4Box {
            kind: data[position + 4..position + 8].try_into()?,
            start: position, payload: position + header..position + size,
        });
        position += size;
    }
    Ok(result)
}

fn required<'a>(children: &'a [Mp4Box], kind: &[u8; 4]) -> Result<&'a Mp4Box> {
    let mut matching = children.iter().filter(|child| &child.kind == kind);
    let found = matching.next().with_context(|| format!("missing MP4 box {}", String::from_utf8_lossy(kind)))?;
    ensure!(matching.next().is_none(), "duplicate MP4 box {}", String::from_utf8_lossy(kind));
    Ok(found)
}

fn children(data: &[u8], parent: &Mp4Box) -> Result<Vec<Mp4Box>> { boxes(data, parent.payload.clone()) }

fn version_offset(data: &[u8], header: &Mp4Box, v0: usize, v1: usize) -> Result<usize> {
    let offset = match data.get(header.payload.start) {
        Some(0) => v0,
        Some(1) => v1,
        _ => anyhow::bail!("unsupported MP4 header version"),
    };
    ensure!(offset + 4 <= header.payload.len(), "truncated MP4 timing header");
    Ok(header.payload.start + offset)
}

// MPEG-4 描述符长度每字节贡献 7 位, 最高位表示仍有后续字节, 最多读取 4 字节.
fn descriptor(data: &[u8], offset: usize, tag: u8) -> Result<Range<usize>> {
    ensure!(data.get(offset) == Some(&tag), "invalid AAC descriptor");
    let mut position = offset + 1;
    let mut length = 0_usize;
    for _ in 0..4 {
        let byte = *data.get(position).context("truncated AAC descriptor length")?;
        position += 1;
        length = (length << 7) | (byte & 127) as usize;
        if byte & 128 == 0 {
            ensure!(length <= data.len() - position, "truncated AAC descriptor");
            return Ok(position..position + length);
        }
    }
    anyhow::bail!("invalid AAC descriptor length")
}

fn sample_description(data: &[u8], stsd: &Mp4Box, video: bool) -> Result<Vec<u8>> {
    let payload = &data[stsd.payload.clone()];
    ensure!(number(payload, 0)? == 0 && number(payload, 4)? == 1, "unsupported MP4 sample descriptions");
    let entries = boxes(payload, 8..payload.len())?;
    ensure!(entries.len() == 1, "expected one MP4 sample entry");
    let entry = &entries[0];
    let fixed = if video { 78 } else { 28 };
    ensure!(entry.kind == if video { *b"avc1" } else { *b"mp4a" }
        && entry.payload.len() >= fixed, "unexpected MP4 sample entry");
    let fields = &payload[entry.payload.start..entry.payload.start + fixed];
    let properties = boxes(payload, entry.payload.start + fixed..entry.payload.end)?;
    let mut signature = Vec::new();
    if video {
        // 编码器名称和码率提示不影响解码, SPS/PPS 和像素描述则必须相容.
        signature.extend_from_slice(&fields[..42]);
        signature.extend_from_slice(&fields[74..]);
        let avcc = &payload[required(&properties, b"avcC")?.payload.clone()];
        ensure!(avcc.len() >= 7 && avcc[0] == 1 && avcc[1] == 100, "expected H.264 High configuration");
        ensure!(avcc[4] & 3 == 3, "expected four-byte AVC NAL lengths");
        signature.extend_from_slice(avcc);
        for kind in [b"pasp", b"colr"] {
            for property in properties.iter().filter(|property| &property.kind == kind) {
                signature.extend_from_slice(kind);
                signature.extend_from_slice(&payload[property.payload.clone()]);
            }
        }
    } else {
        signature.extend_from_slice(fields);
        let esds = &payload[required(&properties, b"esds")?.payload.clone()];
        ensure!(number(esds, 0)? == 0, "unsupported AAC descriptor version");
        let es = descriptor(esds, 4, 3)?;
        ensure!(es.len() >= 3 && esds[es.start + 2] == 0, "unsupported AAC stream descriptor flags");
        let decoder = descriptor(esds, es.start + 3, 4)?;
        ensure!(decoder.end <= es.end && decoder.len() >= 13, "invalid AAC decoder descriptor");
        ensure!(esds[decoder.start] == 0x40 && esds[decoder.start + 1] >> 2 == 5, "expected AAC audio");
        let config = descriptor(esds, decoder.start + 13, 5)?;
        ensure!(config.end <= decoder.end, "AAC configuration exceeds its descriptor");
        signature.extend_from_slice(&esds[config]);
    }
    Ok(signature)
}

impl Init {
    /// 只接受固定输出轨道和时基, 拒绝会重新映射时间轴的 edit list.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let root = boxes(data, 0..data.len())?;
        required(&root, b"ftyp")?;
        let movie = children(data, required(&root, b"moov")?)?;
        let extensions = children(data, required(&movie, b"mvex")?)?;
        let mut tracks = Vec::new();
        for track in movie.iter().filter(|item| item.kind == *b"trak") {
            let track = children(data, track)?;
            ensure!(!track.iter().any(|item| item.kind == *b"edts"), "cache init must not reset track timelines with edit lists");
            let header = required(&track, b"tkhd")?;
            let id = number(data, version_offset(data, header, 12, 20)?)?;
            let media = children(data, required(&track, b"mdia")?)?;
            let header = required(&media, b"mdhd")?;
            let timescale = number(data, version_offset(data, header, 12, 20)?)?;
            let handler = &data[required(&media, b"hdlr")?.payload.clone()];
            let kind = handler.get(8..12).context("truncated MP4 handler")?;
            let video = kind == b"vide";
            ensure!(video || kind == b"soun", "unexpected cached MP4 track");
            ensure!(id == if video { 1 } else { 2 }, "unexpected MP4 track identifier");
            ensure!(timescale == if video { VIDEO_TIMESCALE } else { AUDIO_TIMESCALE }, "unexpected MP4 timescale");
            let information = children(data, required(&media, b"minf")?)?;
            let table = children(data, required(&information, b"stbl")?)?;
            let description = sample_description(data, required(&table, b"stsd")?, video)?;
            let mut defaults = None;
            for extension in extensions.iter().filter(|item| item.kind == *b"trex") {
                let payload = &data[extension.payload.clone()];
                ensure!(payload.len() == 24 && number(payload, 0)? == 0, "invalid MP4 track defaults");
                if number(payload, 4)? == id {
                    ensure!(defaults.is_none(), "duplicate MP4 track defaults");
                    defaults = Some([number(payload, 8)?, number(payload, 12)?,
                        number(payload, 16)?, number(payload, 20)?]);
                }
            }
            let defaults = defaults.context("missing MP4 track defaults")?;
            ensure!(defaults[0] == 1, "unexpected default sample description");
            ensure!(!tracks.iter().any(|track: &Track| track.id == id), "duplicate MP4 track");
            tracks.push(Track { id, timescale, video, description, defaults });
        }
        tracks.sort_by_key(|track| track.id);
        ensure!(!tracks.is_empty() && tracks[0].video && tracks.len() <= 2, "invalid cached MP4 track layout");
        Ok(Self { tracks })
    }

    pub fn audio(&self) -> bool { self.tracks.len() == 2 }

    pub fn compatible(&self, other: &Self) -> Result<()> {
        ensure!(self == other, "resumed init is incompatible with the existing cached init");
        Ok(())
    }
}

// 返回时间区间以及 tfdt/mfhd 的可改写位置, 同时验证样本参数和媒体负载边界.
fn analyze(data: &[u8], init: &Init) -> Result<(Fragment, Vec<(usize, bool)>, usize)> {
    let root = boxes(data, 0..data.len())?;
    let moof = required(&root, b"moof")?;
    let mdat = required(&root, b"mdat")?;
    let fragment = children(data, moof)?;
    let header = required(&fragment, b"mfhd")?;
    ensure!(header.payload.len() == 8 && number(data, header.payload.start)? == 0, "invalid MP4 fragment sequence");
    let sequence_offset = header.payload.start + 4;
    let mut output = Fragment { start: Tail::default(), end: Tail::default(),
        sequence: number(data, sequence_offset)? };
    let mut timestamps = Vec::new();
    let mut byte_ranges = Vec::new();
    for traf in fragment.iter().filter(|item| item.kind == *b"traf") {
        let track_boxes = children(data, traf)?;
        let tfhd = &data[required(&track_boxes, b"tfhd")?.payload.clone()];
        let flags = number(tfhd, 0)?;
        ensure!(flags & !0x02003a == 0 && flags & 0x020000 != 0, "fragment must use moof-relative addressing");
        let id = number(tfhd, 4)?;
        let track = init.tracks.iter().find(|track| track.id == id).context("fragment references an unknown track")?;
        ensure!(!timestamps.iter().any(|(_, video)| *video == track.video), "duplicate fragment track");
        let mut cursor = 8;
        if flags & 2 != 0 {
            ensure!(number(tfhd, cursor)? == 1, "fragment changed sample description");
            cursor += 4;
        }
        // 样本参数按 trex -> tfhd -> trun 逐层覆盖, 缺省值不能跨轨道混用.
        let mut defaults = track.defaults;
        for (flag, value) in [8, 16, 32].into_iter().zip(&mut defaults[1..]) {
            if flags & flag != 0 { *value = number(tfhd, cursor)?; cursor += 4; }
        }
        ensure!(cursor == tfhd.len(), "unexpected fragment track header fields");
        let tfdt = required(&track_boxes, b"tfdt")?;
        ensure!(tfdt.payload.len() == 12 && number(data, tfdt.payload.start)? == 0x01000000,
            "expected a 64-bit MP4 decode timestamp");
        let start = wide(data, tfdt.payload.start + 4)?;
        timestamps.push((tfdt.payload.start + 4, track.video));
        let trun = &data[required(&track_boxes, b"trun")?.payload.clone()];
        let flags = number(trun, 0)?;
        ensure!(flags & !0x000f05 == 0 && flags & 1 != 0, "unsupported MP4 sample run");
        let count = number(trun, 4)? as usize;
        ensure!(count > 0 && count <= 1_000_000, "invalid MP4 sample count");
        let offset = number(trun, 8)? as i32;
        ensure!(offset >= 0, "negative MP4 sample data offset");
        let mut position = moof.start.checked_add(offset as usize).context("sample data offset overflow")?;
        let begin = position;
        let mut cursor = 12;
        let first_flags = if flags & 4 != 0 { let value = number(trun, cursor)?; cursor += 4; value }
            else { defaults[3] };
        let mut duration = 0_u64;
        for sample in 0..count {
            let mut values = [defaults[1], defaults[2], if sample == 0 { first_flags } else { defaults[3] }, 0];
            for (flag, value) in [0x100, 0x200, 0x400, 0x800].into_iter().zip(&mut values) {
                if flags & flag != 0 { *value = number(trun, cursor)?; cursor += 4; }
            }
            // 固定输出禁用 B 帧, 因此不接受非零的显示时间偏移.
            ensure!(values[0] > 0 && values[1] > 0 && values[3] == 0, "invalid MP4 sample timing or size");
            if track.video && sample == 0 {
                ensure!(values[2] & 0x01010000 == 0 && values[2] & 0x02000000 != 0,
                    "video fragment does not start with an independent frame");
            }
            duration = duration.checked_add(values[0] as u64).context("sample duration overflow")?;
            position = position.checked_add(values[1] as usize).context("sample byte offset overflow")?;
        }
        ensure!(cursor == trun.len(), "unexpected MP4 sample fields");
        ensure!(begin >= mdat.payload.start && position <= mdat.payload.end, "samples exceed MP4 media data");
        byte_ranges.push(begin..position);
        let end = start.checked_add(duration).context("decode timestamp overflow")?;
        if track.video { output.start.video = start; output.end.video = end; }
        else { output.start.audio = start; output.end.audio = end; }
    }
    ensure!(timestamps.len() == init.tracks.len(), "fragment is missing a selected track");
    // 各轨样本必须恰好覆盖整个 mdat, 不能重叠, 越界或留下未引用的媒体字节.
    byte_ranges.sort_by_key(|range| range.start);
    let mut position = mdat.payload.start;
    for range in byte_ranges {
        ensure!(range.start == position, "overlapping or unreferenced MP4 samples");
        position = range.end;
    }
    ensure!(position == mdat.payload.end, "unreferenced MP4 media data");
    Ok((output, timestamps, sequence_offset))
}

/// 正式缓存只接受序号和逐轨时间都紧接上一片的分片, 不容忍间隙或重叠.
pub fn inspect(data: &[u8], init: &Init, expected: Tail, sequence: u32) -> Result<Fragment> {
    let (fragment, _, _) = analyze(data, init)?;
    ensure!(fragment.start == expected, "cached MP4 track timeline has a gap or overlap");
    ensure!(fragment.sequence == sequence, "cached MP4 fragment sequence is discontinuous");
    Ok(fragment)
}

/// offset 固定为本次任务的续接点, expected 随正式分片发布前进, 两者不能混用.
pub fn normalize(data: &[u8], init: &Init, offset: Tail, expected: Tail, sequence: u32) -> Result<(Vec<u8>, Fragment)> {
    let mut normalized = Vec::with_capacity(data.len());
    // HLS 不依赖 sidx. 移除索引后, 样本仍通过 moof 相对偏移访问, 无需改写媒体负载.
    for item in boxes(data, 0..data.len())? {
        if item.kind != *b"sidx" { normalized.extend_from_slice(&data[item.start..item.payload.end]); }
    }
    let (_, timestamps, sequence_offset) = analyze(&normalized, init)?;
    for (position, video) in timestamps {
        let time = wide(&normalized, position)?.checked_add(offset.get(video)).context("resume timestamp overflow")?;
        normalized[position..position + 8].copy_from_slice(&time.to_be_bytes());
    }
    normalized[sequence_offset..sequence_offset + 4].copy_from_slice(&sequence.to_be_bytes());
    let fragment = inspect(&normalized, init, expected, sequence)?;
    Ok((normalized, fragment))
}


// ========================================================================== //
// ========================================================================== //
// ========================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    // 只构造解析和续接所需的盒字段, 真正的编解码兼容性由媒体集成测试覆盖.
    fn atom(kind: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        [((payload.len() + 8) as u32).to_be_bytes().as_slice(), kind, &payload].concat()
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_be_bytes()).collect()
    }

    fn descriptor_bytes(tag: u8, payload: Vec<u8>) -> Vec<u8> {
        assert!(payload.len() < 128);
        [vec![tag, payload.len() as u8], payload].concat()
    }

    fn init_fixture(noise: u8, audio: bool) -> Vec<u8> {
        let mut tracks = Vec::new();
        let mut extensions = Vec::new();
        for id in 1..=if audio { 2 } else { 1 } {
            let video = id == 1;
            let mut fields = vec![0; if video { 78 } else { 28 }];
            fields[7] = 1;
            let properties = if video {
                fields[42..74].fill(noise);
                [atom(b"avcC", vec![1, 100, 0, 31, 255, 225, 0, 2, 103, 100, 1, 0, 2, 104, 0]),
                    atom(b"btrt", vec![noise; 12])].concat()
            } else {
                fields[16..18].copy_from_slice(&2_u16.to_be_bytes());
                fields[18..20].copy_from_slice(&16_u16.to_be_bytes());
                fields[24..28].copy_from_slice(&(48_000_u32 << 16).to_be_bytes());
                let mut decoder = vec![0x40, 0x15, 0, 0, 0];
                decoder.extend_from_slice(&[noise; 8]);
                decoder.extend(descriptor_bytes(5, vec![0x11, 0x90]));
                let stream = [vec![0, 2, 0], descriptor_bytes(4, decoder),
                    descriptor_bytes(6, vec![2])].concat();
                atom(b"esds", [words(&[0]), descriptor_bytes(3, stream)].concat())
            };
            let sample = atom(if video { b"avc1" } else { b"mp4a" }, [fields, properties].concat());
            let table = atom(b"stbl", atom(b"stsd", [words(&[0, 1]), sample].concat()));
            let media = atom(b"mdia", [
                atom(b"mdhd", words(&[0, 0, 0, if video { VIDEO_TIMESCALE } else { AUDIO_TIMESCALE }])),
                atom(b"hdlr", [words(&[0, 0]), if video { b"vide".to_vec() } else { b"soun".to_vec() }].concat()),
                atom(b"minf", table),
            ].concat());
            tracks.push(atom(b"trak", [atom(b"tkhd", words(&[0, 0, 0, id])), media].concat()));
            extensions.push(atom(b"trex", words(&[0, id, 1, 0, 0, 0])));
        }
        tracks.push(atom(b"mvex", extensions.concat()));
        [atom(b"ftyp", b"iso6".to_vec()), atom(b"moov", tracks.concat())].concat()
    }

    fn fragment_fixture(audio: bool) -> Vec<u8> {
        let build = |video_offset, audio_offset| {
            let mut tracks = vec![atom(b"mfhd", words(&[0, 1]))];
            for id in 1..=if audio { 2 } else { 1 } {
                let duration = if id == 1 { 3000 } else { 1024 };
                tracks.push(atom(b"traf", [
                    atom(b"tfhd", words(&[0x020000, id])),
                    atom(b"tfdt", [words(&[0x01000000]), 0_u64.to_be_bytes().to_vec()].concat()),
                    atom(b"trun", words(&[0x305, 2, if id == 1 { video_offset } else { audio_offset },
                        0x02000000, duration, 4, duration, 4])),
                ].concat()));
            }
            atom(b"moof", tracks.concat())
        };
        let offset = build(0, 0).len() as u32 + 8;
        [atom(b"styp", b"msdh".to_vec()), atom(b"sidx", vec![0; 16]),
            build(offset, offset + 8), atom(b"mdat", vec![7; if audio { 16 } else { 8 }])].concat()
    }

    fn field(data: &[u8], kind: &[u8; 4], offset: usize) -> usize {
        data.windows(4).position(|value| value == kind).unwrap() + 4 + offset
    }

    #[test]
    fn init_ignores_encoder_labels_and_bitrate_but_requires_decoder_compatibility() {
        let bytes = init_fixture(0, true);
        let init = Init::parse(&bytes).unwrap();
        assert!(init.audio());
        init.compatible(&Init::parse(&init_fixture(9, true)).unwrap()).unwrap();
        assert!(init.compatible(&Init::parse(&init_fixture(0, false)).unwrap()).is_err());
        let mut changed = bytes.clone();
        let index = field(&changed, b"avcC", 3);
        changed[index] += 1;
        assert!(init.compatible(&Init::parse(&changed).unwrap()).is_err());
        let index = field(&changed, b"mdhd", 12);
        changed[index..index + 4].copy_from_slice(&1000_u32.to_be_bytes());
        assert!(Init::parse(&changed).is_err());
    }

    #[test]
    fn normalization_offsets_each_track_without_changing_compressed_samples() {
        for audio in [false, true] {
            let init = Init::parse(&init_fixture(0, audio)).unwrap();
            let data = fragment_fixture(audio);
            let start = Tail { video: 720_000, audio: if audio { 384_000 } else { 0 } };
            let (normalized, fragment) = normalize(&data, &init, start, start, 3).unwrap();
            assert_eq!(fragment.start, start);
            assert_eq!(fragment.end, Tail { video: 726_000, audio: if audio { 386_048 } else { 0 } });
            assert_eq!(fragment.sequence, 3);
            let source_boxes = boxes(&data, 0..data.len()).unwrap();
            let target_boxes = boxes(&normalized, 0..normalized.len()).unwrap();
            assert!(!target_boxes.iter().any(|item| item.kind == *b"sidx"));
            assert_eq!(&data[required(&source_boxes, b"mdat").unwrap().payload.clone()],
                &normalized[required(&target_boxes, b"mdat").unwrap().payload.clone()]);
            assert!(inspect(&normalized, &init, Tail::default(), 3).is_err());
            assert!(inspect(&normalized, &init, start, 4).is_err());
        }
    }

    #[test]
    fn invalid_fragment_runs_and_non_independent_starts_are_rejected() {
        let init = Init::parse(&init_fixture(0, true)).unwrap();
        let valid = fragment_fixture(true);
        inspect(&valid, &init, Tail::default(), 1).unwrap();
        for (kind, offset, value) in [
            (b"tfhd", 4, 99), (b"tfdt", 0, 0), (b"trun", 8, u32::MAX),
            (b"trun", 12, 0x01010000), (b"trun", 16, 0), (b"trun", 20, 5),
        ] {
            let mut changed = valid.clone();
            let index = field(&changed, kind, offset);
            changed[index..index + 4].copy_from_slice(&value.to_be_bytes());
            assert!(inspect(&changed, &init, Tail::default(), 1).is_err(), "{kind:?}:{offset}");
        }
        assert!(inspect(&fragment_fixture(false), &init, Tail::default(), 1).is_err());
        assert!(inspect(&valid[..valid.len() - 1], &init, Tail::default(), 1).is_err());
        let start = Tail { video: u64::MAX, audio: 0 };
        assert!(normalize(&valid, &init, start, start, 1).is_err());
    }

    #[test]
    fn truncated_and_overflowing_boxes_are_rejected() {
        // 长度检查必须限制在当前父盒内, 不能借用相邻盒的字节.
        for data in [
            vec![], vec![0; 7], [12_u32.to_be_bytes().as_slice(), b"moov"].concat(),
            [1_u32.to_be_bytes().as_slice(), b"moov", &u64::MAX.to_be_bytes()].concat(),
        ] {
            assert!(Init::parse(&data).is_err());
        }
        assert!(boxes(&[0; 8], 0..9).is_err());
    }

    #[test]
    fn aac_descriptor_lengths_are_bounded() {
        assert_eq!(descriptor(&[5, 2, 0x11, 0x90], 0, 5).unwrap(), 2..4);
        assert!(descriptor(&[5, 5, 1], 0, 5).is_err());
        assert!(descriptor(&[5, 128, 128, 128, 128, 0], 0, 5).is_err());
    }
}
