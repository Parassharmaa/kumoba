use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const HOUR_MS: u64 = 3_600_000;
pub const RETENTION: Duration = Duration::from_secs(72 * 3600);
pub const MAX_QUERY: usize = 50_000;
pub const MAX_PER_SANDBOX_HOUR: u64 = 10_000;
pub const DROPPED_MARKER_INTERVAL: u64 = 1_000;

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccessEntry {
    pub sandbox_id: String,
    pub labels: BTreeMap<String, String>,
    pub host: String,
    pub port: u16,
    pub decision: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u64,
}

impl AccessEntry {
    pub fn dropped_marker(
        sandbox_id: String,
        labels: BTreeMap<String, String>,
        now_ms: u64,
        dropped: u64,
    ) -> Self {
        Self {
            sandbox_id,
            labels,
            host: String::new(),
            port: 0,
            decision: "dropped".to_string(),
            bytes_in: 0,
            bytes_out: 0,
            started_at_ms: now_ms,
            completed_at_ms: now_ms,
            dropped,
        }
    }

    pub fn new(
        sandbox_id: String,
        labels: BTreeMap<String, String>,
        record: kumoba::AccessRecord,
    ) -> Self {
        Self {
            sandbox_id,
            labels,
            host: record.host,
            port: record.port,
            decision: if record.allowed { "allowed" } else { "denied" }.to_string(),
            bytes_in: record.bytes_in,
            bytes_out: record.bytes_out,
            started_at_ms: epoch_ms(record.started),
            completed_at_ms: epoch_ms(record.completed),
            dropped: 0,
        }
    }
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn hour_of(ms: u64) -> u64 {
    ms / HOUR_MS * HOUR_MS
}

pub struct AccessLog {
    dir: PathBuf,
    current: Mutex<Option<(u64, File)>>,
    written: Mutex<HashMap<(String, u64), u64>>,
}

pub enum Appended {
    Written,
    OverCap,
}

impl AccessLog {
    pub fn open(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            current: Mutex::new(None),
            written: Mutex::new(HashMap::new()),
        })
    }

    fn file_for(&self, hour: u64) -> PathBuf {
        self.dir.join(format!("{hour}.jsonl"))
    }

    pub fn append(&self, entry: &AccessEntry) -> std::io::Result<Appended> {
        let hour = hour_of(entry.completed_at_ms);
        if entry.dropped == 0 {
            let mut written = self.written.lock().unwrap_or_else(|poison| poison.into_inner());
            let count = written.entry((entry.sandbox_id.clone(), hour)).or_insert(0);
            if *count >= MAX_PER_SANDBOX_HOUR {
                return Ok(Appended::OverCap);
            }
            *count += 1;
        }
        self.write(hour, entry)?;
        Ok(Appended::Written)
    }

    fn write(&self, hour: u64, entry: &AccessEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
        line.push(b'\n');
        let mut current = self.current.lock().unwrap_or_else(|poison| poison.into_inner());
        if current.as_ref().is_none_or(|(open, _)| *open != hour) {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.file_for(hour))?;
            *current = Some((hour, file));
        }
        let (_, file) = current.as_mut().expect("opened above");
        file.write_all(&line)
    }

    fn hours(&self) -> std::io::Result<Vec<u64>> {
        let mut hours = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let name = entry?.file_name();
            if let Some(hour) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".jsonl"))
                .and_then(|stem| stem.parse().ok())
            {
                hours.push(hour);
            }
        }
        hours.sort_unstable();
        Ok(hours)
    }

    pub fn query(
        &self,
        since_ms: u64,
        until_ms: u64,
        limit: usize,
    ) -> std::io::Result<(Vec<AccessEntry>, bool)> {
        let limit = limit.clamp(1, MAX_QUERY);
        let mut entries: Vec<AccessEntry> = Vec::new();
        let mut truncated = false;
        let mut cut_inside_group = false;
        'hours: for hour in self.hours()? {
            if hour + HOUR_MS <= since_ms || hour >= until_ms {
                continue;
            }
            let reader = BufReader::new(File::open(self.file_for(hour))?);
            for line in reader.lines() {
                let line = line?;
                let Ok(entry) = serde_json::from_str::<AccessEntry>(&line) else {
                    continue;
                };
                if entry.completed_at_ms < since_ms || entry.completed_at_ms >= until_ms {
                    continue;
                }
                let full = entries.len() >= limit;
                let same_ms_as_last = entries
                    .last()
                    .is_some_and(|last| last.completed_at_ms == entry.completed_at_ms);
                if full && !same_ms_as_last {
                    truncated = true;
                    break 'hours;
                }
                if entries.len() >= 2 * MAX_QUERY {
                    truncated = true;
                    cut_inside_group = true;
                    break 'hours;
                }
                entries.push(entry);
            }
        }
        entries.sort_by_key(|entry| entry.completed_at_ms);
        if cut_inside_group {
            let boundary = entries
                .iter()
                .rposition(|entry| entry.completed_at_ms < entries[entries.len() - 1].completed_at_ms)
                .map(|index| index + 1)
                .unwrap_or(entries.len());
            if boundary > 0 && boundary < entries.len() {
                entries.truncate(boundary);
            }
        }
        Ok((entries, truncated))
    }

    pub fn prune(&self, now_ms: u64, retention: Duration) -> std::io::Result<usize> {
        let cutoff = now_ms.saturating_sub(retention.as_millis() as u64);
        let mut removed = 0;
        for hour in self.hours()? {
            if hour + HOUR_MS <= cutoff {
                std::fs::remove_file(self.file_for(hour))?;
                removed += 1;
            }
        }
        let mut current = self.current.lock().unwrap_or_else(|poison| poison.into_inner());
        if current.as_ref().is_some_and(|(hour, _)| hour + HOUR_MS <= cutoff) {
            *current = None;
        }
        drop(current);
        let mut written = self.written.lock().unwrap_or_else(|poison| poison.into_inner());
        written.retain(|(_, hour), _| hour + HOUR_MS > cutoff);
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(completed_at_ms: u64, host: &str) -> AccessEntry {
        AccessEntry {
            sandbox_id: "sbx".into(),
            labels: BTreeMap::from([("team".to_string(), "org".to_string())]),
            host: host.into(),
            port: 443,
            decision: "allowed".into(),
            bytes_in: 10,
            bytes_out: 2,
            started_at_ms: completed_at_ms - 5,
            completed_at_ms,
            dropped: 0,
        }
    }

    #[test]
    fn appends_per_hour_and_queries_by_completion_time() {
        let dir = tempfile::tempdir().unwrap();
        let log = AccessLog::open(dir.path().join("access")).unwrap();
        log.append(&entry(10 * HOUR_MS + 5, "a.example")).unwrap();
        log.append(&entry(11 * HOUR_MS + 1, "b.example")).unwrap();
        log.append(&entry(10 * HOUR_MS + 50, "c.example")).unwrap();
        assert_eq!(log.hours().unwrap(), vec![10 * HOUR_MS, 11 * HOUR_MS]);

        let (all, truncated) = log.query(0, 12 * HOUR_MS, 100).unwrap();
        assert_eq!(
            all.iter().map(|entry| entry.host.as_str()).collect::<Vec<_>>(),
            ["a.example", "c.example", "b.example"]
        );
        assert!(!truncated);

        let (window, _) = log.query(10 * HOUR_MS + 6, 11 * HOUR_MS, 100).unwrap();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].host, "c.example");

        let (limited, truncated) = log.query(0, 12 * HOUR_MS, 2).unwrap();
        assert_eq!(limited.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn pages_cut_on_millisecond_boundaries_so_cursors_never_skip() {
        let dir = tempfile::tempdir().unwrap();
        let log = AccessLog::open(dir.path().join("access")).unwrap();
        for host in ["a", "b", "c"] {
            log.append(&entry(HOUR_MS + 1, host)).unwrap();
        }
        log.append(&entry(HOUR_MS + 2, "d")).unwrap();
        let (page, truncated) = log.query(0, 2 * HOUR_MS, 2).unwrap();
        assert!(truncated);
        assert_eq!(page.len(), 3);
        assert!(page.iter().all(|entry| entry.completed_at_ms == HOUR_MS + 1));
        let (rest, truncated) = log.query(HOUR_MS + 2, 2 * HOUR_MS, 2).unwrap();
        assert!(!truncated);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].host, "d");
    }

    #[test]
    fn caps_records_per_sandbox_and_hour_but_keeps_drop_markers() {
        let dir = tempfile::tempdir().unwrap();
        let log = AccessLog::open(dir.path().join("access")).unwrap();
        for index in 0..MAX_PER_SANDBOX_HOUR {
            assert!(matches!(
                log.append(&entry(HOUR_MS + index, "flood.example")).unwrap(),
                Appended::Written
            ));
        }
        assert!(matches!(
            log.append(&entry(HOUR_MS + 5, "flood.example")).unwrap(),
            Appended::OverCap
        ));
        assert!(matches!(
            log.append(&entry(2 * HOUR_MS, "next-hour.example")).unwrap(),
            Appended::Written
        ));
        let marker = AccessEntry::dropped_marker("sbx".into(), BTreeMap::new(), 3 * HOUR_MS, 42);
        assert!(matches!(log.append(&marker).unwrap(), Appended::Written));
        let (page, _) = log.query(3 * HOUR_MS, 3 * HOUR_MS + 1, 10).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!((page[0].decision.as_str(), page[0].dropped), ("dropped", 42));
        let serialized = serde_json::to_string(&entry(HOUR_MS, "x")).unwrap();
        assert!(!serialized.contains("dropped"));
    }

    #[test]
    fn prune_drops_whole_hours_past_retention() {
        let dir = tempfile::tempdir().unwrap();
        let log = AccessLog::open(dir.path().join("access")).unwrap();
        log.append(&entry(10 * HOUR_MS + 5, "old.example")).unwrap();
        log.append(&entry(80 * HOUR_MS + 5, "new.example")).unwrap();
        let removed = log.prune(84 * HOUR_MS, RETENTION).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(log.hours().unwrap(), vec![80 * HOUR_MS]);
        log.append(&entry(80 * HOUR_MS + 9, "after.example")).unwrap();
        assert_eq!(log.query(0, 90 * HOUR_MS, 100).unwrap().0.len(), 2);
    }

    #[test]
    fn maps_runtime_records_to_entries() {
        let started = UNIX_EPOCH + Duration::from_millis(1_000);
        let record = kumoba::AccessRecord {
            host: "api.example".into(),
            port: 443,
            allowed: false,
            bytes_in: 0,
            bytes_out: 0,
            started,
            completed: started + Duration::from_millis(7),
        };
        let entry = AccessEntry::new("sbx".into(), BTreeMap::new(), record);
        assert_eq!(entry.decision, "denied");
        assert_eq!((entry.started_at_ms, entry.completed_at_ms), (1_000, 1_007));
    }
}
