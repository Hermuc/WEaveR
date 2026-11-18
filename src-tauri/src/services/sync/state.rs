//! In-memory status cache fed by the we-sync event thread and read by
//! sync_status. Keeping a snapshot here means the UI can render immediately
//! after mount without waiting for the next event.

use std::collections::VecDeque;

use serde::Serialize;

use super::event::SyncEventDto;

const MAX_LOGS: usize = 100;

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncStatus {
    pub running: bool,
    pub paused: bool,
    pub transfer_paused: bool,
    pub busy: bool,
    pub device: Option<super::event::DeviceDto>,
    pub progress: Option<Progress>,
    pub stats: StatsDto,
    pub queue: usize,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub name: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct StatsDto {
    pub uploaded: u64,
    pub skipped: u64,
    pub failed: u64,
    pub filtered: u64,
}

#[derive(Debug, Default)]
pub struct SyncStatusCache {
    inner: SyncStatus,
    logs: VecDeque<String>,
}

impl SyncStatusCache {
    pub fn snapshot(&self) -> SyncStatus {
        let mut out = self.inner.clone();
        out.logs = self.logs.iter().cloned().collect();
        out
    }

    pub fn set_running(&mut self, running: bool) {
        self.inner.running = running;
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.inner.paused = paused;
        if !paused {
            self.inner.progress = None;
        }
    }

    pub fn apply(&mut self, event: &SyncEventDto) {
        match event {
            SyncEventDto::Log { message } => {
                self.logs.push_back(message.clone());
                while self.logs.len() > MAX_LOGS {
                    self.logs.pop_front();
                }
            }
            SyncEventDto::Device { device } => {
                self.inner.device = device.clone();
                if device.is_none() {
                    self.inner.progress = None;
                    self.inner.busy = false;
                }
            }
            SyncEventDto::Busy { value } => {
                self.inner.busy = *value;
                if !*value {
                    self.inner.progress = None;
                }
            }
            SyncEventDto::Progress { name, done, total } => {
                self.inner.progress = Some(Progress {
                    name: name.clone(),
                    done: *done,
                    total: *total,
                });
            }
            SyncEventDto::Stats {
                uploaded,
                skipped,
                failed,
                filtered,
            } => {
                self.inner.stats = StatsDto {
                    uploaded: *uploaded,
                    skipped: *skipped,
                    failed: *failed,
                    filtered: *filtered,
                };
            }
            SyncEventDto::Queue { count } => self.inner.queue = *count,
            SyncEventDto::TransferPaused { value } => self.inner.transfer_paused = *value,
        }
    }
}
