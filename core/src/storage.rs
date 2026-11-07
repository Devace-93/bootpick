//! File access abstraction. Paths are ESP-style (`\EFI\bootpick\...`); the
//! UEFI build maps them to a SimpleFileSystem, the desktop app to a directory.

use alloc::string::String;
use alloc::vec::Vec;

pub trait Storage {
    fn read(&mut self, path: &str) -> Option<Vec<u8>>;
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String>;
    /// (name, is_dir) pairs, sorted, without `.` and `..`.
    fn list_dir(&mut self, path: &str) -> Vec<(String, bool)>;

    fn exists(&mut self, path: &str) -> bool {
        self.read_exists(path)
    }
    /// Default existence check; implementations should override with a
    /// cheaper metadata query.
    fn read_exists(&mut self, path: &str) -> bool {
        self.read(path).is_some()
    }
    fn read_string(&mut self, path: &str) -> Option<String> {
        self.read(path).map(|b| String::from_utf8_lossy(&b).into_owned())
    }
}

/// A storage with nothing in it (used when the config volume is unavailable).
pub struct NoStorage;

impl Storage for NoStorage {
    fn read(&mut self, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn write(&mut self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("no storage".into())
    }
    fn list_dir(&mut self, _: &str) -> Vec<(String, bool)> {
        Vec::new()
    }
}
