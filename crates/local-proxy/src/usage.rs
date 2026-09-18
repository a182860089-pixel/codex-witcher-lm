use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const USAGE_SCHEMA_VERSION: u32 = 1;
const MAX_DAY_BUCKETS: usize = 400;
const MAX_EVENTS: usize = 2_000;
const LOCAL_OFFSET_MS: i64 = 8 * 60 * 60 * 1000;
const MS_PER_DAY: i64 = 86_400_000;
const USD_PER_MILLION_PROMPT: f64 = 1.25;
const USD_PER_MILLION_CACHED: f64 = 0.125;
const USD_PER_MILLION_COMPLETION: f64 = 10.0;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn merge(&mut self, other: Self) {
        self.prompt_tokens = self.prompt_tokens.max(other.prompt_tokens);
        self.completion_tokens = self.completion_tokens.max(other.completion_tokens);
        self.cached_tokens = self.cached_tokens.max(other.cached_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(other.cache_write_tokens);
        self.total_tokens = self.total_tokens.max(other.total_tokens);
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens.saturating_add(self.completion_tokens);
        }
        if self.cached_tokens > self.prompt_tokens {
            self.cached_tokens = self.prompt_tokens;
        }
    }

    fn is_empty(self) -> bool {
        self.prompt_tokens == 0
            && self.completion_tokens == 0
            && self.cached_tokens == 0
            && self.cache_write_tokens == 0
            && self.total_tokens == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UsageEvent {
    id: String,
    started_at_ms: i64,
    status: u16,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UsageDayBucket {
    calls: u64,
    success: u64,
    errors: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageFile {
    schema_version: u32,
    days: BTreeMap<String, UsageDayBucket>,
    events: VecDeque<UsageEvent>,
}

impl Default for UsageFile {
    fn default() -> Self {
        Self {
            schema_version: USAGE_SCHEMA_VERSION,
            days: BTreeMap::new(),
            events: VecDeque::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSeriesPoint {
    pub label: String,
    pub start_ms: i64,
    pub cached_tokens: u64,
    pub uncached_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageHeatCell {
    pub date: String,
    pub weekday: u8,
    pub total_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageOverview {
    pub range: String,
    pub from_ms: i64,
    pub to_ms: i64,
    pub calls: u64,
    pub success: u64,
    pub errors: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub cache_hit_rate: f64,
    pub estimated_usd: f64,
    pub cache_usd: f64,
    pub series: Vec<UsageSeriesPoint>,
    pub heatmap: Vec<UsageHeatCell>,
    pub heat_months: Vec<HeatMonthLabel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeatMonthLabel {
    pub label: String,
    pub column: u32,
}

pub struct UsageStore {
    path: Option<PathBuf>,
    inner: Mutex<UsageFile>,
}

impl UsageStore {
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<UsageFile>(&bytes).ok())
            .filter(|file| file.schema_version == USAGE_SCHEMA_VERSION)
            .unwrap_or_default();
        Self {
            path,
            inner: Mutex::new(file),
        }
    }

    pub fn upsert_from_log(&self, id: &str, started_at_ms: i64, status: u16, usage: TokenUsage) {
        let event = UsageEvent {
            id: id.to_string(),
            started_at_ms: if started_at_ms == 0 {
                now_epoch_ms()
            } else {
                started_at_ms
            },
            status,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cached_tokens: usage.cached_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            total_tokens: if usage.total_tokens == 0 {
                usage.prompt_tokens.saturating_add(usage.completion_tokens)
            } else {
                usage.total_tokens
            },
        };
        let Ok(mut file) = self.inner.lock() else {
            return;
        };
        if let Some(index) = file.events.iter().position(|item| item.id == event.id) {
            let old = file.events.remove(index).expect("index exists");
            subtract_event(&mut file, &old);
        }
        add_event(&mut file, &event);
        file.events.push_front(event);
        while file.events.len() > MAX_EVENTS {
            if let Some(old) = file.events.pop_back() {
                subtract_event(&mut file, &old);
            }
        }
        prune_days(&mut file);
        persist(&file, self.path.as_deref());
    }

    pub fn overview(&self, range: &str, from_ms: Option<i64>, to_ms: Option<i64>) -> UsageOverview {
        let file = self
            .inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        build_overview(&file, range, from_ms, to_ms)
    }
}

pub fn load_overview(
    path: &Path,
    range: &str,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> UsageOverview {
    UsageStore::load(Some(path.to_path_buf())).overview(range, from_ms, to_ms)
}

pub fn extract_token_usage(value: &Value) -> Option<TokenUsage> {
    const PATHS: &[&str] = &[
        "/usage",
        "/response/usage",
        "/response/response/usage",
        "/data/usage",
    ];
    for path in PATHS {
        if let Some(usage) = value.pointer(path).and_then(parse_usage_value) {
            return Some(usage);
        }
    }
    parse_usage_value(value)
}

pub fn extract_token_usage_from_sse(event: &[u8]) -> Option<TokenUsage> {
    crate::agent_loop::sse_data_json(event).and_then(|json| extract_token_usage(&json))
}

fn parse_usage_value(value: &Value) -> Option<TokenUsage> {
    let object = value.as_object()?;
    let prompt =
        json_u64(object.get("prompt_tokens")).or_else(|| json_u64(object.get("input_tokens")));
    let completion =
        json_u64(object.get("completion_tokens")).or_else(|| json_u64(object.get("output_tokens")));
    let total = json_u64(object.get("total_tokens"));
    let cached = json_u64(object.get("cached_tokens"))
        .or_else(|| {
            object
                .get("input_tokens_details")
                .and_then(|details| json_u64(details.get("cached_tokens")))
        })
        .or_else(|| {
            object
                .get("prompt_tokens_details")
                .and_then(|details| json_u64(details.get("cached_tokens")))
        })
        .or_else(|| {
            object
                .get("cache_read_input_tokens")
                .and_then(|value| json_u64(Some(value)))
        });
    let cache_write = json_u64(object.get("cache_creation_input_tokens"))
        .or_else(|| json_u64(object.get("cache_creation_tokens")))
        .or_else(|| json_u64(object.get("cache_write_tokens")))
        .or_else(|| {
            object.get("input_tokens_details").and_then(|details| {
                json_u64(details.get("cache_creation_tokens"))
                    .or_else(|| json_u64(details.get("cache_write_tokens")))
            })
        })
        .or_else(|| {
            object.get("prompt_tokens_details").and_then(|details| {
                json_u64(details.get("cache_creation_tokens"))
                    .or_else(|| json_u64(details.get("cache_write_tokens")))
            })
        });
    if prompt.is_none()
        && completion.is_none()
        && total.is_none()
        && cached.is_none()
        && cache_write.is_none()
    {
        return None;
    }
    let prompt_tokens = prompt.unwrap_or(0);
    let completion_tokens = completion.unwrap_or(0);
    let cached_tokens = cached.unwrap_or(0).min(prompt_tokens);
    let cache_write_tokens = cache_write.unwrap_or(0);
    let total_tokens = total.unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    let usage = TokenUsage {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_write_tokens,
        total_tokens,
    };
    (!usage.is_empty()).then_some(usage)
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| {
            value.as_f64().and_then(|n| {
                if n.is_finite() && n >= 0.0 {
                    Some(n as u64)
                } else {
                    None
                }
            })
        })
}

fn add_event(file: &mut UsageFile, event: &UsageEvent) {
    let key = date_key(event.started_at_ms);
    let bucket = file.days.entry(key).or_default();
    bucket.calls = bucket.calls.saturating_add(1);
    if is_success(event.status) {
        bucket.success = bucket.success.saturating_add(1);
    } else {
        bucket.errors = bucket.errors.saturating_add(1);
    }
    bucket.prompt_tokens = bucket.prompt_tokens.saturating_add(event.prompt_tokens);
    bucket.completion_tokens = bucket
        .completion_tokens
        .saturating_add(event.completion_tokens);
    bucket.cached_tokens = bucket.cached_tokens.saturating_add(event.cached_tokens);
    bucket.cache_write_tokens = bucket
        .cache_write_tokens
        .saturating_add(event.cache_write_tokens);
    bucket.total_tokens = bucket.total_tokens.saturating_add(event.total_tokens);
}

fn subtract_event(file: &mut UsageFile, event: &UsageEvent) {
    let key = date_key(event.started_at_ms);
    let Some(bucket) = file.days.get_mut(&key) else {
        return;
    };
    bucket.calls = bucket.calls.saturating_sub(1);
    if is_success(event.status) {
        bucket.success = bucket.success.saturating_sub(1);
    } else {
        bucket.errors = bucket.errors.saturating_sub(1);
    }
    bucket.prompt_tokens = bucket.prompt_tokens.saturating_sub(event.prompt_tokens);
    bucket.completion_tokens = bucket
        .completion_tokens
        .saturating_sub(event.completion_tokens);
    bucket.cached_tokens = bucket.cached_tokens.saturating_sub(event.cached_tokens);
    bucket.cache_write_tokens = bucket
        .cache_write_tokens
        .saturating_sub(event.cache_write_tokens);
    bucket.total_tokens = bucket.total_tokens.saturating_sub(event.total_tokens);
    if bucket.calls == 0 && bucket.total_tokens == 0 {
        file.days.remove(&key);
    }
}

fn prune_days(file: &mut UsageFile) {
    while file.days.len() > MAX_DAY_BUCKETS {
        if let Some(oldest) = file.days.keys().next().cloned() {
            file.days.remove(&oldest);
        } else {
            break;
        }
    }
}

fn persist(file: &UsageFile, path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec_pretty(file) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn build_overview(
    file: &UsageFile,
    range: &str,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> UsageOverview {
    let now = now_epoch_ms();
    let (from, to, series_kind) = resolve_range(range, from_ms, to_ms, now);
    let mut calls = 0;
    let mut success = 0;
    let mut errors = 0;
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;
    let mut cached_tokens = 0;
    let mut total_tokens = 0;
    let prefer_days = matches!(series_kind, SeriesKind::Day);

    if prefer_days {
        for (key, bucket) in &file.days {
            let start = date_key_to_ms(key);
            if start + MS_PER_DAY - 1 < from || start > to {
                continue;
            }
            calls += bucket.calls;
            success += bucket.success;
            errors += bucket.errors;
            prompt_tokens += bucket.prompt_tokens;
            completion_tokens += bucket.completion_tokens;
            cached_tokens += bucket.cached_tokens;
            total_tokens += bucket.total_tokens;
        }
    } else {
        for event in &file.events {
            if event.started_at_ms < from || event.started_at_ms > to {
                continue;
            }
            calls += 1;
            if is_success(event.status) {
                success += 1;
            } else {
                errors += 1;
            }
            prompt_tokens += event.prompt_tokens;
            completion_tokens += event.completion_tokens;
            cached_tokens += event.cached_tokens;
            total_tokens += event.total_tokens;
        }
    }

    let uncached_prompt = prompt_tokens.saturating_sub(cached_tokens);
    let estimated_usd = million_cost(uncached_prompt, USD_PER_MILLION_PROMPT)
        + million_cost(cached_tokens, USD_PER_MILLION_CACHED)
        + million_cost(completion_tokens, USD_PER_MILLION_COMPLETION);
    let cache_usd = million_cost(cached_tokens, USD_PER_MILLION_CACHED);
    let cache_hit_rate = if prompt_tokens == 0 {
        0.0
    } else {
        (cached_tokens as f64 / prompt_tokens as f64) * 100.0
    };

    UsageOverview {
        range: range.to_string(),
        from_ms: from,
        to_ms: to,
        calls,
        success,
        errors,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        total_tokens,
        cache_hit_rate,
        estimated_usd,
        cache_usd,
        series: build_series(file, from, to, series_kind),
        heatmap: build_heatmap(file, now),
        heat_months: Vec::new(),
    }
    .with_heat_months()
}

impl UsageOverview {
    fn with_heat_months(mut self) -> Self {
        self.heat_months = month_labels(&self.heatmap);
        self
    }
}

#[derive(Clone, Copy)]
enum SeriesKind {
    Minute,
    FiveMinutes,
    Hour,
    Day,
}

fn resolve_range(
    range: &str,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    now: i64,
) -> (i64, i64, SeriesKind) {
    match range {
        "minutes10" => (now.saturating_sub(10 * 60 * 1000), now, SeriesKind::Minute),
        "hour" => (
            now.saturating_sub(60 * 60 * 1000),
            now,
            SeriesKind::FiveMinutes,
        ),
        "day" => {
            let start = start_of_local_day(now);
            (start, now, SeriesKind::Hour)
        }
        "week" => (now.saturating_sub(7 * MS_PER_DAY), now, SeriesKind::Day),
        "custom" => {
            let from = from_ms.unwrap_or_else(|| now.saturating_sub(30 * MS_PER_DAY));
            let to = to_ms.unwrap_or(now).max(from);
            let kind = if to - from <= 2 * MS_PER_DAY {
                SeriesKind::Hour
            } else {
                SeriesKind::Day
            };
            (from, to, kind)
        }
        _ => (now.saturating_sub(30 * MS_PER_DAY), now, SeriesKind::Day),
    }
}

fn build_series(file: &UsageFile, from: i64, to: i64, kind: SeriesKind) -> Vec<UsageSeriesPoint> {
    let (bucket_ms, count) = match kind {
        SeriesKind::Minute => (60_000, 10),
        SeriesKind::FiveMinutes => (5 * 60_000, 12),
        SeriesKind::Hour => {
            let origin = start_of_local_hour(from);
            let last = start_of_local_hour(to);
            let buckets = ((last - origin) / 3_600_000).max(0) as usize + 1;
            (3_600_000, buckets.clamp(1, 48))
        }
        SeriesKind::Day => {
            let start = start_of_local_day(from);
            let end = start_of_local_day(to);
            let days = ((end - start) / MS_PER_DAY).max(0) as usize + 1;
            (MS_PER_DAY, days.clamp(1, 62))
        }
    };
    let origin = match kind {
        SeriesKind::Day => start_of_local_day(from),
        SeriesKind::Hour => start_of_local_hour(from),
        _ => from,
    };
    let mut points: Vec<UsageSeriesPoint> = (0..count)
        .map(|index| {
            let start_ms = origin + index as i64 * bucket_ms;
            UsageSeriesPoint {
                label: series_label(start_ms, kind),
                start_ms,
                cached_tokens: 0,
                uncached_tokens: 0,
                cache_write_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                calls: 0,
            }
        })
        .collect();

    if matches!(kind, SeriesKind::Day) {
        for (key, bucket) in &file.days {
            let start = date_key_to_ms(key);
            if start < origin || start > to {
                continue;
            }
            let index = ((start - origin) / bucket_ms) as usize;
            if let Some(point) = points.get_mut(index) {
                point.cached_tokens = point.cached_tokens.saturating_add(bucket.cached_tokens);
                point.uncached_tokens = point
                    .uncached_tokens
                    .saturating_add(bucket.prompt_tokens.saturating_sub(bucket.cached_tokens));
                point.cache_write_tokens = point
                    .cache_write_tokens
                    .saturating_add(bucket.cache_write_tokens);
                point.completion_tokens = point
                    .completion_tokens
                    .saturating_add(bucket.completion_tokens);
                point.total_tokens = point.total_tokens.saturating_add(bucket.total_tokens);
                point.calls = point.calls.saturating_add(bucket.calls);
            }
        }
        return points;
    }

    for event in &file.events {
        if event.started_at_ms < from || event.started_at_ms > to {
            continue;
        }
        let index = ((event.started_at_ms - origin) / bucket_ms) as usize;
        if let Some(point) = points.get_mut(index) {
            point.cached_tokens = point.cached_tokens.saturating_add(event.cached_tokens);
            point.uncached_tokens = point
                .uncached_tokens
                .saturating_add(event.prompt_tokens.saturating_sub(event.cached_tokens));
            point.cache_write_tokens = point
                .cache_write_tokens
                .saturating_add(event.cache_write_tokens);
            point.completion_tokens = point
                .completion_tokens
                .saturating_add(event.completion_tokens);
            point.total_tokens = point.total_tokens.saturating_add(event.total_tokens);
            point.calls = point.calls.saturating_add(1);
        }
    }
    points
}

fn series_label(start_ms: i64, kind: SeriesKind) -> String {
    let (_year, month, day, hour, minute, weekday) = local_parts(start_ms);
    match kind {
        SeriesKind::Minute | SeriesKind::FiveMinutes => format!("{hour:02}:{minute:02}"),
        SeriesKind::Hour => format!("{hour:02}:00"),
        SeriesKind::Day => {
            if weekday == 0 {
                "周日".to_string()
            } else if weekday == 6 {
                "周六".to_string()
            } else {
                format!("{month}/{day}")
            }
        }
    }
}

fn build_heatmap(file: &UsageFile, now: i64) -> Vec<UsageHeatCell> {
    let today = start_of_local_day(now);
    let weekday = local_weekday(today);
    let end = today + (6 - weekday as i64) * MS_PER_DAY;
    let start = end - 52 * 7 * MS_PER_DAY;
    let mut cells = Vec::with_capacity(53 * 7);
    let mut cursor = start;
    while cursor <= end {
        let date = date_key(cursor);
        let bucket = file.days.get(&date);
        cells.push(UsageHeatCell {
            date,
            weekday: local_weekday(cursor),
            total_tokens: bucket.map(|item| item.total_tokens).unwrap_or(0),
            calls: bucket.map(|item| item.calls).unwrap_or(0),
        });
        cursor += MS_PER_DAY;
    }
    cells
}

fn month_labels(cells: &[UsageHeatCell]) -> Vec<HeatMonthLabel> {
    let mut labels = Vec::new();
    let mut last_month = 0u32;
    for (index, cell) in cells.iter().enumerate() {
        if cell.weekday != 0 {
            continue;
        }
        let column = (index / 7) as u32;
        let month = cell
            .date
            .get(5..7)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        if month == 0 || month == last_month {
            continue;
        }
        last_month = month;
        labels.push(HeatMonthLabel {
            label: format!("{month}月"),
            column,
        });
    }
    labels
}

fn million_cost(tokens: u64, usd_per_million: f64) -> f64 {
    tokens as f64 / 1_000_000.0 * usd_per_million
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn date_key(ms: i64) -> String {
    let (year, month, day, ..) = local_parts(ms);
    format!("{year:04}-{month:02}-{day:02}")
}

fn date_key_to_ms(key: &str) -> i64 {
    let mut parts = key.split('-');
    let year = parts
        .next()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(1970);
    let month = parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    let day = parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    let days = days_from_civil(year, month, day);
    days * MS_PER_DAY - LOCAL_OFFSET_MS
}

fn start_of_local_day(ms: i64) -> i64 {
    let local = ms + LOCAL_OFFSET_MS;
    local.div_euclid(MS_PER_DAY) * MS_PER_DAY - LOCAL_OFFSET_MS
}

fn start_of_local_hour(ms: i64) -> i64 {
    let local = ms + LOCAL_OFFSET_MS;
    local.div_euclid(3_600_000) * 3_600_000 - LOCAL_OFFSET_MS
}

fn local_weekday(ms: i64) -> u8 {
    let days = (ms + LOCAL_OFFSET_MS).div_euclid(MS_PER_DAY);
    ((days + 4).rem_euclid(7)) as u8
}

fn local_parts(ms: i64) -> (i32, u32, u32, u32, u32, u8) {
    let local = ms + LOCAL_OFFSET_MS;
    let days = local.div_euclid(MS_PER_DAY);
    let day_ms = local.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = (day_ms / 3_600_000) as u32;
    let minute = ((day_ms % 3_600_000) / 60_000) as u32;
    let weekday = ((days + 4).rem_euclid(7)) as u8;
    (year, month, day, hour, minute, weekday)
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 }.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_responses_and_chat_usage() {
        let responses = json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 120,
                    "output_tokens": 40,
                    "total_tokens": 160,
                    "input_tokens_details": { "cached_tokens": 80 }
                }
            }
        });
        let usage = extract_token_usage(&responses).expect("responses usage");
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.cached_tokens, 80);
        assert_eq!(usage.cache_write_tokens, 0);
        assert_eq!(usage.total_tokens, 160);

        let chat = json!({
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 4 }
            }
        });
        let usage = extract_token_usage(&chat).expect("chat usage");
        assert_eq!(usage.prompt_tokens, 20);
        assert_eq!(usage.cached_tokens, 4);
        assert_eq!(usage.total_tokens, 25);

        let with_write = json!({
            "usage": {
                "input_tokens": 50,
                "output_tokens": 10,
                "cache_creation_input_tokens": 8,
                "input_tokens_details": { "cached_tokens": 20 }
            }
        });
        let usage = extract_token_usage(&with_write).expect("cache write usage");
        assert_eq!(usage.cached_tokens, 20);
        assert_eq!(usage.cache_write_tokens, 8);
        assert_eq!(usage.completion_tokens, 10);
    }

    #[test]
    fn overview_counts_upserted_events() {
        let store = UsageStore::load(None);
        store.upsert_from_log(
            "req-1",
            now_epoch_ms(),
            200,
            TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                cached_tokens: 80,
                cache_write_tokens: 12,
                total_tokens: 120,
            },
        );
        store.upsert_from_log(
            "req-1",
            now_epoch_ms(),
            200,
            TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                cached_tokens: 80,
                cache_write_tokens: 12,
                total_tokens: 120,
            },
        );
        let overview = store.overview("month", None, None);
        assert_eq!(overview.calls, 1);
        assert_eq!(overview.success, 1);
        assert_eq!(overview.prompt_tokens, 100);
        assert!(overview.cache_hit_rate > 79.0);
        assert_eq!(
            overview
                .series
                .iter()
                .map(|point| point.completion_tokens)
                .sum::<u64>(),
            20
        );
        assert_eq!(
            overview
                .series
                .iter()
                .map(|point| point.uncached_tokens)
                .sum::<u64>(),
            20
        );
        assert_eq!(
            overview
                .series
                .iter()
                .map(|point| point.cache_write_tokens)
                .sum::<u64>(),
            12
        );
        assert!(!overview.heatmap.is_empty());
    }
}
