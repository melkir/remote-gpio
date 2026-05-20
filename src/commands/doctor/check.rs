use super::Status;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Check {
    pub fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            status: Status::Ok,
            detail: None,
        }
    }

    pub fn status(mut self, status: Status) -> Self {
        self.status = status;
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn optional_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    pub fn skipped(mut self) -> Self {
        self.status = Status::Skipped;
        self.detail = None;
        self
    }

    pub fn when(self, condition: bool, ok: Status, fail: Status) -> Self {
        if condition {
            self.status(ok)
        } else {
            self.status(fail)
        }
    }
}

/// Open `path` for read; OK with `path` on success, blocking with error detail on failure.
pub fn readable_file(id: &'static str, label: &'static str, path: &str) -> Check {
    match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(_) => Check::new(id, label).detail(path),
        Err(e) => Check::new(id, label)
            .status(Status::Blocking)
            .detail(format!("{path}: {e}")),
    }
}

/// Open `path` for read+write; OK with `path` on success, blocking on failure.
pub fn read_write_file(id: &'static str, label: &'static str, path: &str) -> Check {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(_) => Check::new(id, label).detail(path),
        Err(e) => Check::new(id, label)
            .status(Status::Blocking)
            .detail(format!("{path}: {e}")),
    }
}
