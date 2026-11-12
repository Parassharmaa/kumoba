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
    written: Mutex<HashMap<(String, String, u64), u64>>,
}

pub enum Appended {
    Written,
    OverCap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub hour: u64,
    pub line: u64,
}

impl Cursor {
    pub fn parse(value: &str) -> Option<Self> {
        let (hour, line) = value.split_once(':')?;
        Some(Self {
            hour: hour.parse().ok()?,
            line: line.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.hour, self.line)
    }
}

pub struct Page {
    pub entries: Vec<AccessEntry>,
    pub truncated: bool,
    pub next_cursor: Option<Cursor>,
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
            let key = (entry.sandbox_id.clone(), entry.decision.clone(), hour);
            let count = written.entry(key).or_insert(0);
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
        cursor: Option<Cursor>,
    ) -> std::io::Result<Page> {
        let limit = limit.clamp(1, MAX_QUERY);
        let mut entries = Vec::new();
        let mut last = cursor;
        let mut truncated = false;
        'hours: for hour in self.hours()? {
            if hour + HOUR_MS <= since_ms || hour >= until_ms {
                continue;
            }
            if cursor.is_some_and(|cursor| hour < cursor.hour) {
                continue;
            }
            let reader = BufReader::new(File::open(self.file_for(hour))?);
            for (index, line) in reader.lines().enumerate() {
                let line = line?;
                let position = Cursor { hour, line: index as u64 };
                if cursor.is_some_and(|cursor| hour == cursor.hour && position.line <= cursor.line)
                {
                    continue;
                }
                let Ok(entry) = serde_json::from_str::<AccessEntry>(&line) else {
                    last = Some(position);
                    continue;
                };
                if entry.completed_at_ms < since_ms || entry.completed_at_ms >= until_ms {
                    last = Some(position);
                    continue;
                }
                if entries.len() >= limit {
                    truncated = true;
                    break 'hours;
                }
                entries.push(entry);
                last = Some(position);
            }
        }
        Ok(Page {
            entries,
            truncated,
            next_cursor: truncated.then_some(last).flatten(),
        })
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
        written.retain(|(_, _, hour), _| hour + HOUR_MS > cutoff);
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

    fn hosts(page: &Page) -> Vec<&str> {
        page.entries.iter().map(|entry| entry.host.as_str()).collect()
    }

    #[test]
    fn appends_per_hour_and_queries_by_completion_time() {
        let dir = tempfile::tempdir().unwrap();
        let log = AccessLog::open(dir.path().join("access")).unwrap();
        log.append(&entry(10 * HOUR_MS + 5, "a.example")).unwrap();
        log.append(&entry(11 * HOUR_MS + 1, "b.example")).unwrap();
        log.append(&entry(10 * HOUR_MS + 50, "c.example")).unwrap();
        assert_eq!(log.hours().unwrap(), vec![10 * HOUR_MS, 11 * HOUR_MS]);

        let all = log.query(0, 12 * HOUR_MS, 100, None).unwrap();
        assert_eq!(hosts(&all), ["a.example", "c.example", "b.example"]);
        assert!(!all.truncated);
        assert_eq!(all.next_cursor, None);

        let window = log.query(10 * HOUR_MS + 6, 11 * HOUR_MS, 100, None).unwrap();
        assert_eq!(hosts(&window), ["c.example"]);
    }

    #[test]
    fn cursor_pages_never_skip_or_repeat_even_with_out_of_order_times() {
        let dir = tempfile::tempdir().unwrap();
        let log = AccessLog::open(dir.path().join("access")).unwrap();
        for (host, at) in [("a", 3), ("b", 1), ("c", 1), ("d", 2)] {
            log.append(&entry(HOUR_MS + at, host)).unwrap();
        }
        log.append(&entry(2 * HOUR_MS, "e")).unwrap();
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = log.query(0, 3 * HOUR_MS, 2, cursor).unwrap();
            seen.extend(page.entries.iter().map(|entry| entry.host.clone()));
            if !page.truncated {
                break;
            }
            cursor = page.next_cursor;
            assert!(cursor.is_some());
        }
        assert_eq!(seen, ["a", "b", "c", "d", "e"]);
        let parsed = Cursor::parse(&Cursor { hour: HOUR_MS, line: 1 }.to_string()).unwrap();
        assert_eq!(parsed, Cursor { hour: HOUR_MS, line: 1 });
        assert_eq!(Cursor::parse("junk"), None);
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
        let mut denied = entry(HOUR_MS + 5, "169.254.169.254");
        denied.decision = "denied".into();
        assert!(matches!(log.append(&denied).unwrap(), Appended::Written));
        assert!(matches!(
            log.append(&entry(2 * HOUR_MS, "next-hour.example")).unwrap(),
            Appended::Written
        ));
        let marker = AccessEntry::dropped_marker("sbx".into(), BTreeMap::new(), 3 * HOUR_MS, 42);
        assert!(matches!(log.append(&marker).unwrap(), Appended::Written));
        let page = log.query(3 * HOUR_MS, 3 * HOUR_MS + 1, 10, None).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(
            (page.entries[0].decision.as_str(), page.entries[0].dropped),
            ("dropped", 42)
        );
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
        assert_eq!(log.query(0, 90 * HOUR_MS, 100, None).unwrap().entries.len(), 2);
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
