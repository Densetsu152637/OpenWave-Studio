//! The recording writer round trip.

use ow_media::RecordWriter;

#[test]
fn pushed_frames_land_in_the_wav_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.wav");
    let (writer, mut producer) = RecordWriter::start(&path, 2, 48_000, 1 << 16).unwrap();
    let mut expected = Vec::new();
    for chunk in 0..200_u32 {
        let block: Vec<f32> = (0..2 * 100)
            .map(|i| (chunk * 1000 + i) as f32 * 1e-6)
            .collect();
        assert_eq!(producer.push(&block), 100);
        expected.extend(block);
    }
    let stats = writer.finish().unwrap();
    assert_eq!((stats.frames, stats.dropped_frames), (20_000, 0));
    let mut reader = hound::WavReader::open(&path).unwrap();
    assert_eq!(
        (reader.spec().channels, reader.spec().sample_rate),
        (2, 48_000)
    );
    let read: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(read, expected);
}

#[test]
fn a_full_ring_drops_whole_frames_and_counts_them() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, mut producer) =
        RecordWriter::start(&dir.path().join("t.wav"), 2, 48_000, 64).unwrap();
    let block = vec![0.5_f32; 2 * 1000];
    let accepted = producer.push(&block);
    assert!(accepted <= 64);
    let stats = writer.finish().unwrap();
    assert_eq!(stats.frames, accepted as u64);
    assert_eq!(stats.dropped_frames, (1000 - accepted) as u64);
}
