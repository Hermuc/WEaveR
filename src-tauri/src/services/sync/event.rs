//! DTO conversion for events emitted by the we-sync engine.
//!
//! The we-sync crate stays UI-agnostic; this module is the only place that
//! knows how its Event enum is shaped, so an upstream change has one adapter
//! to update.

use serde::Serialize;
use we_sync::engine::Event;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncEventDto {
    Log { message: String },
    Device { device: Option<DeviceDto> },
    Busy { value: bool },
    Progress { name: String, done: u64, total: u64 },
    Stats { uploaded: u64, skipped: u64, failed: u64, filtered: u64 },
    Queue { count: usize },
    TransferPaused { value: bool },
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceDto {
    pub channel: String,
    pub name: String,
    pub storages: Vec<String>,
}

pub fn to_dto(event: Event) -> SyncEventDto {
    match event {
        Event::Log(message) => SyncEventDto::Log { message },
        Event::Device(device) => SyncEventDto::Device {
            device: device.map(|d| DeviceDto {
                channel: d.channel.as_str().to_string(),
                name: d.name,
                storages: d.storages,
            }),
        },
        Event::Busy(value) => SyncEventDto::Busy { value },
        Event::Progress { name, done, total } => SyncEventDto::Progress { name, done, total },
        Event::Stats(stats) => SyncEventDto::Stats {
            uploaded: stats.uploaded,
            skipped: stats.skipped,
            failed: stats.failed,
            filtered: stats.filtered,
        },
        Event::Queue(count) => SyncEventDto::Queue { count },
        Event::TransferPaused(value) => SyncEventDto::TransferPaused { value },
    }
}
