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

    /// Leave the check as-is when `condition` holds; otherwise drop to `fail`
    /// and explain why. A passing check keeps its default empty detail, so the
    /// summary prints its label alone.
    pub fn unless(self, condition: bool, fail: Status, detail: impl Into<String>) -> Self {
        if condition {
            self
        } else {
            self.status(fail).detail(detail)
        }
    }
}

/// Open `path` for read; OK with `path` on success, blocking with error detail on failure.
pub fn readable_file(id: &'static str, label: &'static str, path: &str) -> Check {
    open_file(id, label, path, std::fs::OpenOptions::new().read(true))
}

/// Open `path` for read+write; OK with `path` on success, blocking on failure.
pub fn read_write_file(id: &'static str, label: &'static str, path: &str) -> Check {
    open_file(
        id,
        label,
        path,
        std::fs::OpenOptions::new().read(true).write(true),
    )
}

fn open_file(
    id: &'static str,
    label: &'static str,
    path: &str,
    options: &std::fs::OpenOptions,
) -> Check {
    match options.open(path) {
        Ok(_) => Check::new(id, label).detail(path),
        Err(e) => Check::new(id, label)
            .status(Status::Blocking)
            .detail(format!("{path}: {e}")),
    }
}
