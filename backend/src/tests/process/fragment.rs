//! 校验 fMP4 分片解析, 续接和越界防护的测试.

use super::*;

// 编码器标签和码率差异可忽略, 解码参数或时间基变化必须拒绝.
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

// 续接只改写每条轨道的时间偏移, 压缩样本字节保持不变.
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

// 非法 trun 数据和非关键帧起始的分片必须拒绝.
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

// 截断或长度溢出的盒不能导致越界读取.
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

// AAC 描述符长度字段必须有界, 拒绝超长编码.
#[test]
fn aac_descriptor_lengths_are_bounded() {
    assert_eq!(descriptor(&[5, 2, 0x11, 0x90], 0, 5).unwrap(), 2..4);
    assert!(descriptor(&[5, 5, 1], 0, 5).is_err());
    assert!(descriptor(&[5, 128, 128, 128, 128, 0], 0, 5).is_err());
}

// ========================================================================== //

// 构造 MP4 盒: 4 字节长度 + 类型 + 负载.
fn atom(kind: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
    [((payload.len() + 8) as u32).to_be_bytes().as_slice(), kind, &payload].concat()
}

// 按大端序拼接 32 位整数.
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_be_bytes()).collect()
}

// 构造短格式长度的 ES 描述符.
fn descriptor_bytes(tag: u8, payload: Vec<u8>) -> Vec<u8> {
    assert!(payload.len() < 128);
    [vec![tag, payload.len() as u8], payload].concat()
}

// 只构造解析和续接所需的初始化盒字段, noise 模拟编码器标签差异.
fn init_fixture(noise: u8, audio: bool) -> Vec<u8> {
    let mut tracks = Vec::new();
    let mut extensions = Vec::new();
    for track_id in 1..=if audio { 2 } else { 1 } {
        let video = track_id == 1;
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
        tracks.push(atom(b"trak", [atom(b"tkhd", words(&[0, 0, 0, track_id])), media].concat()));
        extensions.push(atom(b"trex", words(&[0, track_id, 1, 0, 0, 0])));
    }
    tracks.push(atom(b"mvex", extensions.concat()));
    [atom(b"ftyp", b"iso6".to_vec()), atom(b"moov", tracks.concat())].concat()
}

// 构造带正确 data_offset 的单分片, 可选附带音轨.
fn fragment_fixture(audio: bool) -> Vec<u8> {
    let build = |video_offset, audio_offset| {
        let mut tracks = vec![atom(b"mfhd", words(&[0, 1]))];
        for track_id in 1..=if audio { 2 } else { 1 } {
            let duration = if track_id == 1 { 3000 } else { 1024 };
            tracks.push(atom(b"traf", [
                atom(b"tfhd", words(&[0x020000, track_id])),
                atom(b"tfdt", [words(&[0x01000000]), 0_u64.to_be_bytes().to_vec()].concat()),
                atom(b"trun", words(&[0x305, 2, if track_id == 1 { video_offset } else { audio_offset },
                    0x02000000, duration, 4, duration, 4])),
            ].concat()));
        }
        atom(b"moof", tracks.concat())
    };
    let offset = build(0, 0).len() as u32 + 8;
    [atom(b"styp", b"msdh".to_vec()), atom(b"sidx", vec![0; 16]),
        build(offset, offset + 8), atom(b"mdat", vec![7; if audio { 16 } else { 8 }])].concat()
}

// 定位指定盒类型之后偏移处的字段位置.
fn field(data: &[u8], kind: &[u8; 4], offset: usize) -> usize {
    data.windows(4).position(|value| value == kind).unwrap() + 4 + offset
}
