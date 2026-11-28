//! `gth_record::write_dataset`: shards written from an offline dataset's arrays.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gth_record::{DatasetMeta, DatasetSamples, ShardMeta, write_dataset};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("gth-dataset-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn members(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let data = std::fs::read(path).unwrap();
    let archive = rawzip::ZipArchive::from_slice(&data).unwrap();
    let mut members = BTreeMap::new();
    let mut entries = archive.entries();
    while let Some(entry) = entries.next_entry().unwrap() {
        assert_eq!(entry.compression_method(), rawzip::CompressionMethod::STORE);
        let name = String::from_utf8_lossy(entry.file_path().as_ref()).into_owned();
        let local = archive.get_entry(entry.wayfinder()).unwrap();
        members.insert(name, local.data().to_vec());
    }
    members
}

fn npy_data(member: &[u8]) -> &[u8] {
    let header_len = u16::from_le_bytes([member[8], member[9]]) as usize;
    &member[10 + header_len..]
}

const META: &str = r#"{
    "source_id": "synthetic-ckpt0", "episode_id": 3, "game_id": "atari-pong", "fps": 60.0,
    "frameskip": 4, "obs_size": [2, 3], "crop": null, "orientation": 0,
    "interpolation": "area", "maxpool": true, "grayscale": true,
    "ram_names": ["reward", "terminal"], "terminated": true,
    "dataset": {"name": "synthetic", "first": 10}
}"#;

struct Arrays {
    frames: Vec<u8>,
    direction: Vec<u8>,
    buttons: Vec<u8>,
    ram: Vec<i64>,
    episode_frame: Vec<u64>,
    segment: Vec<u32>,
}

impl Arrays {
    fn new(n: usize) -> Arrays {
        Arrays {
            frames: (0..n * 6).map(|i| i as u8).collect(),
            direction: (0..n).map(|i| (i % 9) as u8).collect(),
            buttons: (0..n).map(|i| (i % 2) as u8).collect(),
            ram: (0..n)
                .flat_map(|i| [i as i64 - 1, i64::from(i + 1 == n)])
                .collect(),
            episode_frame: (0..n).map(|i| 4 * i as u64).collect(),
            segment: vec![0; n],
        }
    }

    fn samples(&self) -> DatasetSamples<'_> {
        DatasetSamples {
            frames: &self.frames,
            direction: &self.direction,
            buttons: &self.buttons,
            ram: &self.ram,
            episode_frame: &self.episode_frame,
            segment: &self.segment,
        }
    }
}

fn chunk(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

#[test]
fn a_dataset_shard_holds_the_arrays_in_chunks() {
    let dir = TempDir::new();
    let out = dir.0.join("d.npz");
    let meta = DatasetMeta::from_json(META, &out).unwrap();
    let arrays = Arrays::new(5);
    let harness = BTreeMap::from([("version".to_owned(), "test".to_owned())]);
    let summary = write_dataset(&meta, &arrays.samples(), chunk(2), &harness, &out).unwrap();
    assert_eq!(
        (summary.samples, summary.frames, summary.frame_chunks),
        (5, 0, 3)
    );
    assert_eq!(summary.bytes, std::fs::metadata(&out).unwrap().len());
    assert!(!dir.0.join("d.npz.partial").exists());

    let members = members(&out);
    let shard: ShardMeta = serde_json::from_slice(&members["meta.json"]).unwrap();
    assert_eq!(shard.shard_version, gth_record::SHARD_VERSION);
    assert_eq!(
        (shard.label_source.as_str(), shard.filter.as_str()),
        ("dataset", "all")
    );
    assert_eq!((shard.samples, shard.frames, shard.channels), (5, 0, 1));
    assert_eq!((shard.episode_id, shard.terminated), (3, true));
    assert!(shard.recording.is_none() && shard.sprite_hw.is_none());
    assert_eq!(shard.dataset.unwrap()["first"], 10);
    assert_eq!(shard.harness, harness);
    let mut frames = Vec::new();
    for c in 0..3 {
        frames.extend(zstd::decode_all(members[&format!("frames_{c:05}.zst")].as_slice()).unwrap());
    }
    assert_eq!(frames, arrays.frames);
    assert_eq!(npy_data(&members["direction.npy"]), arrays.direction);
    let ram: Vec<u8> = arrays.ram.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(npy_data(&members["ram.npy"]), ram);
    let confidence: Vec<u8> = (0..5).flat_map(|_| 1f32.to_le_bytes()).collect();
    assert_eq!(npy_data(&members["confidence.npy"]), confidence);
    assert!(npy_data(&members["frame_direction.npy"]).is_empty());
}

/// A change that makes the arrays invalid.
type Change = fn(&mut Arrays);

#[test]
fn bad_arrays_and_meta_leave_no_shard() {
    let dir = TempDir::new();
    let out = dir.0.join("d.npz");
    let meta = DatasetMeta::from_json(META, &out).unwrap();
    let harness = BTreeMap::new();
    let cases: [(&str, Change); 5] = [
        ("frame bytes", |a| a.frames.pop().map(drop).unwrap()),
        ("ram has 5 values", |a| a.ram.truncate(5)),
        ("direction 9", |a| a.direction[1] = 9),
        ("buttons 16", |a| a.buttons[0] = 16),
        ("segments", |a| a.segment = vec![0, 1, 1, 0]),
    ];
    for (message, change) in cases {
        let mut arrays = Arrays::new(4);
        change(&mut arrays);
        let error = write_dataset(&meta, &arrays.samples(), chunk(2), &harness, &out).unwrap_err();
        assert!(
            matches!(error, gth_record::Error::Mismatch { .. }),
            "{error}"
        );
        assert!(error.to_string().contains(message), "{message}: {error}");
        assert!(!out.exists());
    }
    let error = DatasetMeta::from_json(&META.replace("\"episode_id\": 3,", ""), &out).unwrap_err();
    assert!(error.to_string().contains("episode_id"), "{error}");
    let error = DatasetMeta::from_json(&META.replace("\"fps\"", "\"extra\": 1, \"fps\""), &out)
        .unwrap_err();
    assert!(error.to_string().contains("extra"), "{error}");
}
