//! Messages between the settings app (user) and its privileged helper (root),
//! one JSON object per line over the helper's stdin/stdout.

use crate::host::Detection;
use bootpick_core::scan::{Entry, Kind, Scan, Volume};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Copy \EFI\bootpick to `staging` (owned by the user) and scan.
    Load { staging: String },
    /// Copy the allowed files from `staging` back to the ESP.
    Save { staging: String },
    SetEnabled { enabled: bool },
    Uninstall,
    Quit,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EntryJ {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub icon: String,
    pub kind: KindJ,
    pub hidden_default: bool,
    pub location: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum KindJ {
    Efi { vol: usize, path: String },
    Bls { vol: usize, sd_boot: String, entry: String },
    BootNext(u16),
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ScanJ {
    pub volumes: Vec<(String, String)>,
    pub entries: Vec<EntryJ>,
    pub fw_setup_supported: bool,
}

impl From<&Scan> for ScanJ {
    fn from(s: &Scan) -> Self {
        ScanJ {
            volumes: s.volumes.iter().map(|v| (v.key.clone(), v.disk.clone())).collect(),
            entries: s
                .entries
                .iter()
                .map(|e| EntryJ {
                    id: e.id.clone(),
                    title: e.title.clone(),
                    detail: e.detail.clone(),
                    icon: e.icon.clone(),
                    kind: match &e.kind {
                        Kind::Efi { vol, path } => KindJ::Efi { vol: *vol, path: path.clone() },
                        Kind::LinuxBls { vol, sd_boot, entry } => KindJ::Bls { vol: *vol, sd_boot: sd_boot.clone(), entry: entry.clone() },
                        Kind::BootNext(n) => KindJ::BootNext(*n),
                    },
                    hidden_default: e.hidden_default,
                    location: e.location.clone(),
                })
                .collect(),
            fw_setup_supported: s.fw_setup_supported,
        }
    }
}

impl From<&ScanJ> for Scan {
    fn from(s: &ScanJ) -> Self {
        Scan {
            volumes: s.volumes.iter().map(|(k, d)| Volume { key: k.clone(), disk: d.clone() }).collect(),
            entries: s
                .entries
                .iter()
                .map(|e| Entry {
                    id: e.id.clone(),
                    title: e.title.clone(),
                    detail: e.detail.clone(),
                    icon: e.icon.clone(),
                    kind: match &e.kind {
                        KindJ::Efi { vol, path } => Kind::Efi { vol: *vol, path: path.clone() },
                        KindJ::Bls { vol, sd_boot, entry } => Kind::LinuxBls { vol: *vol, sd_boot: sd_boot.clone(), entry: entry.clone() },
                        KindJ::BootNext(n) => Kind::BootNext(*n),
                    },
                    hidden_default: e.hidden_default,
                    location: e.location.clone(),
                })
                .collect(),
            fw_setup_supported: s.fw_setup_supported,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LoadReply {
    /// Device holding bootpick (e.g. /dev/sda9) and a short label.
    pub esp_device: String,
    pub esp_label: String,
    pub scan: ScanJ,
    pub detections: BTreeMap<String, Detection>,
    /// Firmware entry "bootpick": number, active, first in BootOrder.
    pub boot_entry: Option<(u16, bool, bool)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    Ok { load: Option<LoadReply>, message: String },
    Error { message: String },
}
