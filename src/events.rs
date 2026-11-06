//! Progress events: one JSON object per line on the backend's stdout, read by the GUI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    /// A new step started (`index` counts from 0).
    Step { index: usize, total: usize, title: String },
    /// Overall progress, 0.0–1.0.
    Progress { value: f64 },
    /// A line for the details log.
    Log { text: String },
    /// Finished successfully.
    Done,
    /// Stopped with an error.
    Error { text: String },
}

impl Event {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"type\":\"log\",\"text\":\"?\"}".into())
    }

    pub fn from_line(line: &str) -> Option<Event> {
        serde_json::from_str(line.trim()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip() {
        for e in [
            Event::Step { index: 2, total: 9, title: "Copying Tom OS".into() },
            Event::Progress { value: 0.5 },
            Event::Log { text: "$ mkfs.ext4".into() },
            Event::Done,
            Event::Error { text: "boom".into() },
        ] {
            assert_eq!(Event::from_line(&e.to_line()), Some(e));
        }
        assert_eq!(Event::Done.to_line(), "{\"type\":\"done\"}");
        assert_eq!(Event::from_line("not json"), None);
    }
}
