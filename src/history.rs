use std::{fs, path::PathBuf};

use crate::{
    domain::{DeliveryStatus, HistoryRecord},
    error::{AppError, Result},
};

pub const MAX_HISTORY: usize = 10;

pub trait HistoryStore: Send {
    fn append(&mut self, record: HistoryRecord) -> Result<()>;
    fn update_delivery(
        &mut self,
        id: &str,
        status: DeliveryStatus,
        backend: Option<String>,
    ) -> Result<()>;
    fn list(&self, limit: usize) -> Result<Vec<HistoryRecord>>;
}

#[derive(Clone, Debug)]
pub struct JsonHistoryStore {
    path: PathBuf,
}

impl JsonHistoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn read_all(&self) -> Result<Vec<HistoryRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let contents = fs::read_to_string(&self.path)?;
        serde_json::from_str(&contents).map_err(AppError::from)
    }

    fn write_all(&self, records: &[HistoryRecord]) -> Result<()> {
        let parent = self.path.parent().ok_or_else(|| {
            AppError::History(format!(
                "history path has no parent: {}",
                self.path.display()
            ))
        })?;
        fs::create_dir_all(parent)?;

        let payload = serde_json::to_vec_pretty(records)?;
        let temporary = parent.join(format!(".history-{}.tmp", std::process::id()));
        fs::write(&temporary, payload)?;
        fs::rename(&temporary, &self.path)?;
        Ok(())
    }
}

impl HistoryStore for JsonHistoryStore {
    fn append(&mut self, record: HistoryRecord) -> Result<()> {
        let mut records = self.read_all()?;
        records.insert(0, record);
        records.truncate(MAX_HISTORY);
        self.write_all(&records)
    }

    fn update_delivery(
        &mut self,
        id: &str,
        status: DeliveryStatus,
        backend: Option<String>,
    ) -> Result<()> {
        let mut records = self.read_all()?;
        let record = records
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or_else(|| AppError::History(format!("history record not found: {id}")))?;
        record.delivery_status = status;
        record.delivery_backend = backend;
        self.write_all(&records)
    }

    fn list(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        let mut records = self.read_all()?;
        records.truncate(limit.min(MAX_HISTORY));
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::domain::{DeliveryStatus, HistoryRecord};

    #[test]
    fn keeps_only_the_newest_ten_records() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("history.json");
        let mut store = JsonHistoryStore::new(&path);

        for index in 0..12 {
            store
                .append(HistoryRecord::new(
                    index.to_string(),
                    10,
                    "model".to_owned(),
                    format!("text {index}"),
                ))
                .expect("append record");
        }

        let records = store.list(10).expect("list records");
        assert_eq!(records.len(), 10);
        assert_eq!(records[0].id, "11");
        assert_eq!(records[9].id, "2");
    }

    #[test]
    fn updates_delivery_without_changing_transcript() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("history.json");
        let mut store = JsonHistoryStore::new(&path);
        store
            .append(HistoryRecord::new(
                "one".to_owned(),
                10,
                "model".to_owned(),
                "hello".to_owned(),
            ))
            .expect("append record");

        store
            .update_delivery(
                "one",
                DeliveryStatus::CopiedToClipboard,
                Some("wl-copy".to_owned()),
            )
            .expect("update delivery");

        let record = store.list(1).expect("list records").remove(0);
        assert_eq!(record.transcript, "hello");
        assert_eq!(record.delivery_status, DeliveryStatus::CopiedToClipboard);
        assert_eq!(record.delivery_backend.as_deref(), Some("wl-copy"));
    }
}
