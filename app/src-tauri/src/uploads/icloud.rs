//! macOS: the iCloud Drive upload flag of one file, through NSURL resource
//! values. Safe objc2 calls only.

use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSArray, NSNumber, NSString, NSURL};

use super::{UploadFlag, UploadReader};

// The resource keys by value: each constant's string is its own name, and
// reading the extern statics themselves would need `unsafe`.
const IS_UBIQUITOUS: &str = "NSURLIsUbiquitousItemKey";
const IS_UPLOADED: &str = "NSURLUbiquitousItemIsUploadedKey";

#[derive(Debug, Default, Clone, Copy)]
pub struct ICloudReader;

impl UploadReader for ICloudReader {
    fn supported(&self) -> bool {
        true
    }

    fn read(&self, file: &Path) -> UploadFlag {
        let Some(path) = file.to_str() else {
            return UploadFlag::Unknown;
        };
        // This runs on a plain thread: drain what Foundation autoreleases.
        autoreleasepool(|_| {
            // A new URL every time: an NSURL caches its resource values.
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            let ubiquitous = NSString::from_str(IS_UBIQUITOUS);
            let uploaded = NSString::from_str(IS_UPLOADED);
            let keys = NSArray::from_slice(&[&*ubiquitous, &*uploaded]);
            let Ok(values) = url.resourceValuesForKeys_error(&keys) else {
                return UploadFlag::Unknown;
            };
            let flag = |key: &NSString| {
                values
                    .objectForKey(key)
                    .and_then(|value| value.downcast::<NSNumber>().ok())
                    .map(|number| number.boolValue())
            };
            match (flag(&ubiquitous), flag(&uploaded)) {
                (Some(true), Some(true)) => UploadFlag::Uploaded,
                (Some(true), Some(false)) => UploadFlag::Waiting,
                _ => UploadFlag::Unknown,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_outside_icloud_is_not_known() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("operator.jsonl");
        std::fs::write(&file, b"{}\n").expect("write");
        assert_eq!(ICloudReader.read(&file), UploadFlag::Unknown);
        assert_eq!(
            ICloudReader.read(&dir.path().join("missing.jsonl")),
            UploadFlag::Unknown
        );
    }

    /// Manual measurement, never part of a normal run: set
    /// `AGENDAR_UPLOAD_PROBE_DIR` to a scratch folder inside iCloud Drive.
    /// Writes one file there and prints when its flag turns uploaded.
    #[test]
    #[ignore = "needs a scratch folder inside iCloud Drive"]
    fn probe_a_real_upload() {
        let Some(dir) = std::env::var_os("AGENDAR_UPLOAD_PROBE_DIR") else {
            return;
        };
        let file = Path::new(&dir).join("probe.jsonl");
        for round in 0..3 {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
                .expect("open");
            handle.write_all(&[b'x'; 1200]).expect("write");
            handle.write_all(b"\n").expect("write");
            handle.sync_data().expect("sync");
            drop(handle);
            let start = std::time::Instant::now();
            let mut delay = super::super::FIRST_DELAY;
            let mut reads = 0_u32;
            let mut spent = std::time::Duration::ZERO;
            loop {
                let before = std::time::Instant::now();
                let flag = ICloudReader.read(&file);
                spent += before.elapsed();
                reads += 1;
                println!(
                    "round {round} t={:.1}s read {reads}: {flag:?} ({:?})",
                    start.elapsed().as_secs_f32(),
                    before.elapsed()
                );
                if flag != UploadFlag::Waiting || start.elapsed().as_secs() > 180 {
                    break;
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(super::super::MAX_DELAY);
            }
            println!("round {round}: {reads} reads, {spent:?} in reads");
        }
    }
}
