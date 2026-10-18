//! Each account's remaining quota from the OAuth usage endpoint, refreshing the token into
//! Claude's own store. Ported from ai-usagebar.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::creds;
use crate::paths::{account_config_dir, config_dir, home};
use crate::registry::Registry;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's public OAuth client id (not a secret).
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const BETA: &str = "oauth-2025-04-20";
/// The usage endpoint rate-limits hard unless the request carries a Claude Code
/// `User-Agent` — a real published release, as ai-usagebar pins it.
const USAGE_USER_AGENT: &str = "claude-cli/2.1.281 (external, cli)";
const REFRESH_USER_AGENT: &str = "claude-cli/1.0";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
pub const CACHE_TTL_SECS: i64 = 15 * 60;
const REFRESH_BUFFER_SECS: i64 = 300;

pub const PRIMARY_LABEL: &str = "claude";

/// `config_dir: None` is the primary.
pub struct Account {
    pub label: String,
    pub config_dir: Option<PathBuf>,
    pub reserve: u8,
}

pub fn accounts() -> Vec<Account> {
    let registry = Registry::load();
    let mut all = vec![Account {
        label: PRIMARY_LABEL.into(),
        config_dir: None,
        reserve: registry.reserve(PRIMARY_LABEL),
    }];
    all.extend(registry.labels.iter().map(|label| Account {
        label: label.clone(),
        config_dir: Some(account_config_dir(label)),
        reserve: registry.reserve(label),
    }));
    all
}

/// The accounts `auto` may pick: every one but those excluded from it.
pub fn auto_accounts() -> Vec<Account> {
    let registry = Registry::load();
    accounts()
        .into_iter()
        .filter(|a| registry.in_auto(&a.label))
        .collect()
}

/// `used` is a percentage, 0–100.
pub struct Window {
    pub name: String,
    pub used: f64,
    pub resets_at: Option<i64>,
}

impl Window {
    /// A window whose reset time has passed since it was measured counts as empty.
    pub fn used_at(&self, now: i64) -> f64 {
        if self.resets_at.is_some_and(|r| r <= now) {
            0.0
        } else {
            self.used
        }
    }
}

pub struct Report {
    pub label: String,
    pub primary: bool,
    pub email: Option<String>,
    pub logged_in: bool,
    pub windows: Vec<Window>,
    pub fetched_at: Option<i64>,
    pub error: Option<String>,
    pub reserve: u8,
}

impl Report {
    pub fn pressure(&self, now: i64) -> Option<f64> {
        pressure(&self.windows, now)
    }
}

pub fn pressure(windows: &[Window], now: i64) -> Option<f64> {
    pressure_for(windows, now, None)
}

fn pressure_for(windows: &[Window], now: i64, model: Option<&str>) -> Option<f64> {
    windows
        .iter()
        .filter(|w| applies(w, model))
        .map(|w| w.used_at(now))
        .fold(None, |acc, u| Some(acc.map_or(u, |a: f64| a.max(u))))
}

/// From here a run is likely to hit the limit and move, so the account waits
/// behind every account with more room.
const NEAR_LIMIT: f64 = 95.0;
/// A weekly window with no reset time has not started yet.
const WEEK_SECS: i64 = 7 * 86_400;
const MIN_HOURS_LEFT: f64 = 0.25;

/// A model-scoped weekly cap (`7d fable`) limits only runs on that model;
/// with no model known, every window counts.
fn applies(w: &Window, model: Option<&str>) -> bool {
    let family = w
        .name
        .strip_prefix("7d ")
        .and_then(|f| f.split_whitespace().next());
    match (family, model) {
        (Some(family), Some(model)) => model.to_lowercase().contains(family),
        _ => true,
    }
}

/// Percent of the binding weekly window still unused, per hour until it resets.
/// Weekly quota left at the reset is lost, so the highest rate is spent first.
pub fn drain_rate(r: &Report, now: i64, model: Option<&str>) -> Option<f64> {
    drain_rate_held(r, now, model, 0.0)
}

/// [`drain_rate`] with `held` more percent counted as used, for a reserve.
fn drain_rate_held(r: &Report, now: i64, model: Option<&str>, held: f64) -> Option<f64> {
    r.windows
        .iter()
        .filter(|w| w.name.starts_with("7d") && applies(w, model))
        .map(|w| {
            let left = (100.0 - w.used_at(now) - held).max(0.0);
            let secs = w
                .resets_at
                .filter(|t| *t > now)
                .map_or(WEEK_SECS, |t| t - now);
            (left, left / (secs as f64 / 3600.0).max(MIN_HOURS_LEFT))
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal))
        .map(|(_, rate)| rate)
}

pub fn choose_for(reports: &[Report], now: i64, model: Option<&str>) -> Option<usize> {
    choose_among(reports, now, model, |_| true)
}

/// The best of the reports `keep` accepts for a run on `model`, as an index
/// into all of them: room in every window that limits the run, then the
/// highest [`drain_rate`], then the least pressure. Accounts near a limit, at
/// one, and with unknown quota follow, in that order. A reserve counts as that
/// many more points used, but never ranks an account behind one at its limit.
pub fn choose_among(
    reports: &[Report],
    now: i64,
    model: Option<&str>,
    keep: impl Fn(&Report) -> bool,
) -> Option<usize> {
    let fresh_week = 100.0 / (WEEK_SECS as f64 / 3600.0);
    let rank = |r: &Report| match pressure_for(&r.windows, now, model) {
        None => (3u8, 0.0, 0.0),
        Some(p) if p >= 100.0 => (2, p, 0.0),
        Some(p) => {
            let reserve = f64::from(r.reserve);
            let held = p + reserve;
            (
                u8::from(held >= NEAR_LIMIT),
                -drain_rate_held(r, now, model, reserve).unwrap_or(fresh_week),
                held,
            )
        }
    };
    reports
        .iter()
        .enumerate()
        .filter(|(_, r)| r.logged_in && keep(r))
        .min_by(|(_, a), (_, b)| rank(a).partial_cmp(&rank(b)).unwrap_or(Ordering::Equal))
        .map(|(i, _)| i)
}

/// A run moves between turns only when the best account drains its weekly
/// quota this many times as fast as the run's account, and by this many points
/// an hour: a move re-reads the conversation without the prompt cache.
const MOVE_RATIO: f64 = 1.5;
const MOVE_MIN_GAP: f64 = 0.1;
/// Runs do not move for balance onto an account whose 5-hour window is this
/// full: the moves themselves would push it over.
const MOVE_MAX_5H: f64 = 70.0;

/// A move [`rebalance_target`] recommends. `urgent` when the run's account is
/// near or at a limit; otherwise the move is for balance.
#[derive(Debug, PartialEq)]
pub struct Move {
    pub to: String,
    pub why: String,
    pub urgent: bool,
}

/// Where a run on `current` should move between turns, if anywhere, among the
/// accounts `skip` does not rule out: the best account for `model` when
/// `current` is near or at a limit and the best is not, or when the best drains
/// its weekly quota much faster and has room in its 5-hour window.
pub fn rebalance_target(
    reports: &[Report],
    now: i64,
    model: Option<&str>,
    current: &str,
    skip: impl Fn(&str) -> bool,
) -> Option<Move> {
    let cur = reports.iter().find(|r| r.label == current)?;
    let best = &reports[choose_among(reports, now, model, |r| {
        r.label == current || !skip(&r.label)
    })?];
    if best.label == current {
        return None;
    }
    let (cur_p, best_p) = (
        pressure_for(&cur.windows, now, model)?,
        pressure_for(&best.windows, now, model)?,
    );
    let to = best.label.clone();
    for (limit, why) in [
        (100.0, "is at its limit"),
        (NEAR_LIMIT, "is near its limit"),
    ] {
        if cur_p >= limit && best_p < limit {
            let why = why.to_string();
            return Some(Move {
                to,
                why,
                urgent: true,
            });
        }
    }
    let five_hour = best
        .windows
        .iter()
        .find(|w| w.name == "5h")
        .map_or(0.0, |w| w.used_at(now));
    if best_p >= NEAR_LIMIT || five_hour >= MOVE_MAX_5H {
        return None;
    }
    let (b, c) = (drain_rate(best, now, model)?, drain_rate(cur, now, model)?);
    (b >= c * MOVE_RATIO && b - c >= MOVE_MIN_GAP).then(|| Move {
        why: format!("spends weekly quota slower ({c:.2}%/h left to its reset, {to} {b:.2}%/h)"),
        to,
        urgent: false,
    })
}

/// The model a `claude` command line asks for: `--model x` or `--model=x`.
pub fn model_arg<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Option<String> {
    let mut it = args.iter().map(|a| a.as_ref().to_string_lossy());
    while let Some(a) = it.next() {
        if a == "--model" {
            return it.next().map(|m| m.into_owned());
        }
        if let Some(m) = a.strip_prefix("--model=") {
            return Some(m.to_string());
        }
    }
    None
}

pub fn reports(accounts: &[Account], force: bool) -> Vec<Report> {
    reports_with(accounts, |a| report(a, force))
}

/// Like [`reports`], from the cache only, whatever its age: no network.
pub fn cached_reports(accounts: &[Account]) -> Vec<Report> {
    reports_with(accounts, |a| lookup(a, Fetch::Never))
}

#[derive(Clone, Copy, PartialEq)]
enum Fetch {
    Never,
    IfStale,
    Always,
}

fn reports_with(accounts: &[Account], lookup: impl Fn(&Account) -> Report + Sync) -> Vec<Report> {
    let lookup = &lookup;
    std::thread::scope(|s| {
        let handles: Vec<_> = accounts
            .iter()
            .map(|a| s.spawn(move || lookup(a)))
            .collect();
        handles
            .into_iter()
            .zip(accounts)
            .map(|(h, a)| {
                h.join().unwrap_or_else(|_| Report {
                    label: a.label.clone(),
                    primary: a.config_dir.is_none(),
                    email: None,
                    logged_in: true,
                    windows: Vec::new(),
                    fetched_at: None,
                    error: Some("quota lookup failed".into()),
                    reserve: a.reserve,
                })
            })
            .collect()
    })
}

pub fn report(account: &Account, force: bool) -> Report {
    lookup(account, if force { Fetch::Always } else { Fetch::IfStale })
}

fn lookup(account: &Account, fetch: Fetch) -> Report {
    let now = now_secs();
    let cache = cache_path(&account.label);
    let cached = read_cache(&cache);
    let mut r = Report {
        label: account.label.clone(),
        primary: account.config_dir.is_none(),
        email: email(account.config_dir.as_deref()),
        logged_in: true,
        windows: Vec::new(),
        fetched_at: None,
        error: None,
        reserve: account.reserve,
    };
    let apply_cache = |r: &mut Report| {
        if let Some((at, usage)) = &cached {
            r.windows = parse_usage(usage);
            r.fetched_at = Some(*at);
        }
    };

    let source = match creds::source_for(account.config_dir.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            r.error = Some(e);
            apply_cache(&mut r);
            return r;
        }
    };
    // Always check the login, even on a cache hit, so a logged-out account is
    // never picked on stale figures.
    let blob = match creds::read(&source) {
        Ok(Some(b)) if !signed_out(&b) => b,
        Ok(_) => {
            // Claude leaves the last identity in `.claude.json` after a logout.
            r.logged_in = false;
            r.email = None;
            return r;
        }
        Err(e) => {
            r.error = Some(e);
            apply_cache(&mut r);
            return r;
        }
    };

    let fresh = cached
        .as_ref()
        .is_some_and(|(at, _)| now - at < CACHE_TTL_SECS);
    if fetch == Fetch::Never || (fresh && fetch == Fetch::IfStale) {
        apply_cache(&mut r);
        return r;
    }

    match fetch_live(account, &source, &blob, now) {
        Ok(usage) => {
            write_cache(&cache, now, &usage);
            r.windows = parse_usage(&usage);
            r.fetched_at = Some(now);
        }
        Err(e) => {
            r.error = Some(e);
            apply_cache(&mut r);
        }
    }
    r
}

fn fetch_live(
    account: &Account,
    source: &creds::Source,
    blob: &str,
    now: i64,
) -> Result<Value, String> {
    let mut doc: Value =
        serde_json::from_str(blob).map_err(|_| "unreadable credential".to_string())?;
    if needs_refresh(&doc, now) {
        doc = refresh_locked(account, source, now)?;
    }
    let token = oauth_str(&doc, "accessToken").ok_or("no access token in the credential")?;
    get_usage(&token, &account.label)
}

/// A credential item can outlive its login with blank tokens.
fn signed_out(blob: &str) -> bool {
    serde_json::from_str::<Value>(blob).is_ok_and(|doc| {
        oauth_str(&doc, "accessToken").is_none() && oauth_str(&doc, "refreshToken").is_none()
    })
}

fn oauth_str(doc: &Value, key: &str) -> Option<String> {
    doc.get("claudeAiOauth")?
        .get(key)?
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// A credential with no refresh token (Claude's trusted-device flow) is used as-is:
/// posting an empty grant only fails.
fn needs_refresh(doc: &Value, now: i64) -> bool {
    let expires = doc
        .get("claudeAiOauth")
        .and_then(|o| o.get("expiresAt"))
        .and_then(Value::as_f64)
        .map(|ms| (ms / 1000.0) as i64);
    oauth_str(doc, "refreshToken").is_some()
        && expires.is_some_and(|e| e < now + REFRESH_BUFFER_SECS)
}

/// Re-reads under the lock in case Claude or another claude-proxy already refreshed. A rotated
/// refresh token that fails to save leaves the account logged out, so that is a hard error.
fn refresh_locked(account: &Account, source: &creds::Source, now: i64) -> Result<Value, String> {
    let lock_path = cache_path(&account.label).with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        let _ = crate::paths::private_dir(parent);
    }
    let lock = crate::paths::append(&lock_path)
        .map_err(|e| format!("could not create the refresh lock: {e}"))?;
    lock.lock()
        .map_err(|e| format!("could not take the refresh lock: {e}"))?;

    let blob = creds::read(source)?.ok_or("the account was logged out")?;
    let mut doc: Value =
        serde_json::from_str(&blob).map_err(|_| "unreadable credential".to_string())?;
    if !needs_refresh(&doc, now) {
        return Ok(doc);
    }
    let refresh_token = oauth_str(&doc, "refreshToken").ok_or("no refresh token")?;
    let (access, rotated, expires_in) = post_refresh(&refresh_token, &account.label)?;

    let oauth = doc
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
        .ok_or("unreadable credential")?;
    oauth.insert("accessToken".into(), json!(access));
    if let Some(rt) = &rotated {
        oauth.insert("refreshToken".into(), json!(rt));
    }
    oauth.insert("expiresAt".into(), json!(now * 1000 + expires_in * 1000));

    if let Err(e) = creds::write(source, &doc.to_string()) {
        if rotated.is_some() {
            return Err(format!(
                "refreshed the login but could not save it ({e}); run `{}` to sign in again",
                account.label
            ));
        }
        // Only the access token changed: losing it costs one more refresh.
    }
    Ok(doc)
}

fn post_refresh(refresh_token: &str, label: &str) -> Result<(String, Option<String>, i64), String> {
    let resp = ureq::post(TOKEN_URL)
        .timeout(HTTP_TIMEOUT)
        .set("Content-Type", "application/json")
        .set("anthropic-beta", BETA)
        .set("User-Agent", REFRESH_USER_AGENT)
        .send_json(json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh_token,
        }));
    match resp {
        Ok(r) => {
            let v: Value = r
                .into_json()
                .map_err(|_| "unreadable token refresh response".to_string())?;
            let access = v
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or("token refresh returned no access token")?
                .to_string();
            let rotated = v
                .get("refresh_token")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let expires_in = v
                .get("expires_in")
                .and_then(Value::as_f64)
                .map_or(3600, |f| f as i64);
            Ok((access, rotated, expires_in))
        }
        Err(ureq::Error::Status(code, _)) => Err(format!(
            "token refresh failed (HTTP {code}); run `{label}` to sign in again"
        )),
        Err(ureq::Error::Transport(t)) => Err(format!("token refresh failed: {}", t.kind())),
    }
}

fn get_usage(token: &str, label: &str) -> Result<Value, String> {
    let resp = ureq::get(USAGE_URL)
        .timeout(HTTP_TIMEOUT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", BETA)
        .set("User-Agent", USAGE_USER_AGENT)
        .set("Content-Type", "application/json")
        .call();
    match resp {
        Ok(r) => r
            .into_json()
            .map_err(|_| "unreadable usage response".to_string()),
        Err(ureq::Error::Status(401, _)) => {
            Err(format!("sign-in expired; run `{label}` to log in again"))
        }
        Err(ureq::Error::Status(429, _)) => Err("rate limited by the usage endpoint".into()),
        Err(ureq::Error::Status(code, _)) => Err(format!("usage lookup failed (HTTP {code})")),
        Err(ureq::Error::Transport(t)) => Err(format!("usage lookup failed: {}", t.kind())),
    }
}

/// Lossy by design: the endpoint is undocumented and its shape varies by plan.
pub fn parse_usage(v: &Value) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    for (key, name) in [
        ("five_hour", "5h"),
        ("seven_day", "7d"),
        ("seven_day_opus", "7d opus"),
        ("seven_day_sonnet", "7d sonnet"),
    ] {
        let Some(w) = v.get(key).filter(|w| w.is_object()) else {
            continue;
        };
        if let Some(used) = percent(w.get("utilization")) {
            out.push(Window {
                name: name.into(),
                used,
                resets_at: resets(w.get("resets_at")),
            });
        }
    }
    // Model-scoped weekly caps that have no dedicated `seven_day_*` field.
    for limit in v
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if limit.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
            continue;
        }
        let Some(model) = limit
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let name = format!("7d {}", model.to_lowercase());
        if out.iter().any(|w| w.name == name) {
            continue;
        }
        if let Some(used) = percent(limit.get("percent")) {
            out.push(Window {
                name,
                used,
                resets_at: resets(limit.get("resets_at")),
            });
        }
    }
    out
}

/// RFC 3339 from the usage endpoint, or Unix seconds from a live
/// `rate_limit_event` folded into the cache.
fn resets(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => parse_rfc3339(s),
        _ => None,
    }
}

fn percent(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn claude_json(config_dir: Option<&Path>) -> PathBuf {
    match config_dir {
        Some(dir) => dir.join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

fn login(path: &Path, key: &str) -> Option<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    v.pointer(&format!("/oauthAccount/{key}"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn email(config_dir: Option<&Path>) -> Option<String> {
    login(&claude_json(config_dir), "emailAddress")
}

/// Account and organization: the pair a quota belongs to.
type Identity = (String, String);

fn identity_in(path: &Path) -> Option<Identity> {
    Some((
        login(path, "accountUuid")?,
        login(path, "organizationUuid")?,
    ))
}

/// The Claude session this process runs under. The desktop app puts it in the
/// environment; a terminal session is whoever its config dir is logged in as.
pub(crate) fn caller() -> Option<Identity> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let (Some(account), Some(org)) = (
        var("CLAUDE_CODE_ACCOUNT_UUID"),
        var("CLAUDE_CODE_ORGANIZATION_UUID"),
    ) {
        return Some((account, org));
    }
    std::env::var_os("CLAUDECODE")?;
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    identity_in(&claude_json(dir.as_deref()))
}

/// Whether `label` is the account the calling Claude session already runs on.
pub fn is_callers(label: &str) -> bool {
    let Some(caller) = caller() else {
        return false;
    };
    accounts()
        .iter()
        .find(|a| a.label == label)
        .and_then(|a| identity_in(&claude_json(a.config_dir.as_deref())))
        .is_some_and(|id| id == caller)
}

/// Mark that something happened to an account now, for every run to see:
/// `refused` when it refused a turn, `moved-in` when a run moved onto it.
pub fn stamp(label: &str, kind: &str) {
    stamp_at(&cache_path(label).with_extension(kind), now_secs());
}

/// Whether [`stamp`] marked `kind` on the account in the last `secs` seconds.
pub fn stamped_within(label: &str, kind: &str, secs: i64) -> bool {
    stamped_within_at(&cache_path(label).with_extension(kind), now_secs(), secs)
}

fn stamp_at(path: &Path, now: i64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, now.to_string());
}

fn stamped_within_at(path: &Path, now: i64, secs: i64) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .is_some_and(|at| now - at < secs)
}

pub fn invalidate(label: &str) {
    let _ = std::fs::remove_file(cache_path(label));
}

pub fn record_rate_limit(label: &str, info: &Value) {
    record_rate_limit_at(&cache_path(label), info, now_secs());
}

fn record_rate_limit_at(path: &Path, info: &Value, now: i64) {
    let Some(windows) = info.get("unifiedWindows") else {
        return;
    };
    let mut usage = read_cache(path)
        .map(|(_, u)| u)
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut changed = false;
    for key in ["five_hour", "seven_day"] {
        let Some(w) = windows.get(key) else {
            continue;
        };
        let Some(used) = w.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        usage[key] = json!({ "utilization": used * 100.0, "resets_at": w.get("resetsAt") });
        changed = true;
    }
    if changed {
        write_cache(path, now, &usage);
    }
}

fn cache_path(label: &str) -> PathBuf {
    config_dir()
        .join("cache")
        .join("quota")
        .join(format!("{label}.json"))
}

fn read_cache(path: &Path) -> Option<(i64, Value)> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some((v.get("fetched_at")?.as_i64()?, v.get("usage")?.clone()))
}

/// Best-effort and atomic: a failed cache write only costs a refetch.
fn write_cache(path: &Path, fetched_at: i64, usage: &Value) {
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = crate::paths::private_dir(parent);
    let body = json!({ "fetched_at": fetched_at, "usage": usage }).to_string();
    let _ = crate::paths::write_atomic(path, body.as_bytes(), 0o600);
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse().ok() };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let offset = match *b.get(i)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let o = num(i + 1, i + 3)? * 3600 + num(i + 4, i + 6)? * 60;
            if sign == b'+' {
                o
            } else {
                -o
            }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn short_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else if m == 0 && secs > 0 {
        "<1m".into()
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(name: &str, used: f64, resets_at: Option<i64>) -> Window {
        Window {
            name: name.into(),
            used,
            resets_at,
        }
    }

    fn choose(reports: &[Report], now: i64) -> Option<usize> {
        choose_for(reports, now, None)
    }

    fn report(label: &str, logged_in: bool, windows: Vec<Window>) -> Report {
        Report {
            label: label.into(),
            primary: false,
            email: None,
            logged_in,
            windows,
            fetched_at: None,
            error: None,
            reserve: 0,
        }
    }

    #[test]
    fn one_failed_lookup_does_not_sink_the_others() {
        let accounts: Vec<Account> = ["a", "boom", "c"]
            .into_iter()
            .map(|label| Account {
                label: label.into(),
                config_dir: Some(PathBuf::from(label)),
                reserve: 0,
            })
            .collect();
        let got = reports_with(&accounts, |a| {
            assert!(a.label != "boom", "lookup blew up");
            report(&a.label, true, vec![])
        });
        let labels: Vec<&str> = got.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["a", "boom", "c"]);
        assert!(got[1].error.is_some() && got[1].logged_in);
        assert!(got[0].error.is_none() && got[2].error.is_none());
    }

    #[test]
    fn rfc3339_known_vectors() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-01-01T00:00:00Z"), Some(946_684_800));
        assert_eq!(
            parse_rfc3339("2000-01-01T02:00:00.123456+02:00"),
            Some(946_684_800)
        );
        assert_eq!(
            parse_rfc3339("1999-12-31T19:00:00-05:00"),
            Some(946_684_800)
        );
        assert_eq!(parse_rfc3339("2026-10-02"), None);
        assert_eq!(parse_rfc3339("not a date at all!!!"), None);
    }

    #[test]
    fn parse_usage_reads_windows_scoped_caps_and_skips_nulls() {
        let v = json!({
            "five_hour": {"utilization": 12.0, "resets_at": "2000-01-01T00:00:00Z"},
            "seven_day": {"utilization": 30, "resets_at": null},
            "seven_day_opus": null,
            "limits": [
                {"kind": "weekly_scoped", "percent": 55,
                 "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "five_hour", "percent": 12}
            ]
        });
        let w = parse_usage(&v);
        let names: Vec<_> = w.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["5h", "7d", "7d fable"]);
        assert_eq!(w[0].used, 12.0);
        assert_eq!(w[0].resets_at, Some(946_684_800));
        assert_eq!(w[1].resets_at, None);
        assert_eq!(w[2].used, 55.0);
    }

    #[test]
    fn pressure_is_the_fullest_window_not_yet_reset() {
        let now = 1_000;
        let ws = vec![
            window("5h", 90.0, Some(500)),
            window("7d", 40.0, Some(5_000)),
        ];
        assert_eq!(pressure(&ws, now), Some(40.0));
        assert_eq!(pressure(&[], now), None);
    }

    #[test]
    fn an_identity_needs_both_the_account_and_its_organization() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join(".claude.json");
        std::fs::write(
            &file,
            r#"{"oauthAccount":{"accountUuid":"a1","organizationUuid":"o1","emailAddress":"x@y"}}"#,
        )
        .unwrap();
        assert_eq!(identity_in(&file), Some(("a1".into(), "o1".into())));
        std::fs::write(
            &file,
            r#"{"oauthAccount":{"accountUuid":"a1","organizationUuid":""}}"#,
        )
        .unwrap();
        assert_eq!(identity_in(&file), None);
        assert_eq!(identity_in(&t.path().join("missing.json")), None);
    }

    #[test]
    fn choose_among_skips_what_it_leaves_out_and_indexes_into_all() {
        let reports = vec![
            report("roomy", true, vec![window("5h", 5.0, None)]),
            report("busy", true, vec![window("5h", 80.0, None)]),
        ];
        assert_eq!(choose(&reports, 0), Some(0));
        assert_eq!(
            choose_among(&reports, 0, None, |r| r.label != "roomy"),
            Some(1)
        );
        assert_eq!(choose_among(&reports, 0, None, |_| false), None);
    }

    #[test]
    fn choose_prefers_least_pressure_known_quota_and_skips_logged_out() {
        let now = 0;
        let reports = vec![
            report("busy", true, vec![window("5h", 80.0, None)]),
            report("out", false, vec![window("5h", 0.0, None)]),
            report("unknown", true, vec![]),
            report("idle", true, vec![window("5h", 10.0, None)]),
        ];
        assert_eq!(choose(&reports, now), Some(3));
        assert_eq!(choose(&reports[..3], now), Some(0));
        assert_eq!(choose(&reports[1..2], now), None);
    }

    #[test]
    fn a_reserve_ranks_an_account_as_that_much_fuller_but_never_behind_a_full_one() {
        let mut mine = report("mine", true, vec![window("5h", 40.0, None)]);
        mine.reserve = 25;
        let other = |used| report("other", true, vec![window("5h", used, None)]);
        assert_eq!(choose(&[mine, other(60.0)], 0), Some(1));
        let mut mine = report("mine", true, vec![window("5h", 30.0, None)]);
        mine.reserve = 25;
        assert_eq!(choose(&[mine, other(60.0)], 0), Some(0));
        let mut mine = report("mine", true, vec![window("5h", 90.0, None)]);
        mine.reserve = 25;
        assert_eq!(choose(&[mine, other(100.0)], 0), Some(0));
    }

    #[test]
    fn a_credential_with_blank_tokens_is_signed_out() {
        assert!(signed_out(
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#
        ));
        assert!(signed_out(r#"{"mcpOAuth":{}}"#));
        assert!(!signed_out(r#"{"claudeAiOauth":{"accessToken":"a"}}"#));
        assert!(!signed_out(
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"r"}}"#
        ));
        assert!(!signed_out("not json"));
    }

    #[test]
    fn ties_go_to_the_emptier_weekly_window() {
        let reports = vec![
            report(
                "a",
                true,
                vec![window("5h", 50.0, None), window("7d", 50.0, None)],
            ),
            report(
                "b",
                true,
                vec![window("5h", 50.0, None), window("7d", 20.0, None)],
            ),
        ];
        assert_eq!(choose(&reports, 0), Some(1));
    }

    #[test]
    fn drains_the_weekly_quota_that_expires_soonest() {
        let h = 3600;
        let reports = vec![
            // 90% left over 120h: 0.75%/h.
            report("fresh", true, vec![window("7d", 10.0, Some(120 * h))]),
            // 30% left, gone in 2h: 15%/h.
            report("expiring", true, vec![window("7d", 70.0, Some(2 * h))]),
        ];
        assert_eq!(choose(&reports, 0), Some(1));
        assert!((drain_rate(&reports[0], 0, None).unwrap() - 0.75).abs() < 1e-9);
        // After its reset, a whole new week drains slower than the old one.
        assert!(drain_rate(&reports[1], 3 * h, None).unwrap() < 0.6);
        assert_eq!(choose(&reports, 3 * h), Some(0));
    }

    #[test]
    fn a_reserve_counts_as_used_weekly_quota_too() {
        let h = 3600;
        let mine = |reserve| {
            let mut r = report("mine", true, vec![window("7d", 20.0, Some(10 * h))]);
            r.reserve = reserve;
            r
        };
        let other = || report("other", true, vec![window("7d", 40.0, Some(10 * h))]);
        // 80%/10h beats 60%/10h; held 25 points it is 55%/10h and loses.
        assert_eq!(choose(&[mine(0), other()], 0), Some(0));
        assert_eq!(choose(&[mine(25), other()], 0), Some(1));
        assert_eq!(choose(&[mine(15), other()], 0), Some(0));
        // `list` shows the real rate, not the held one.
        assert!((drain_rate(&mine(25), 0, None).unwrap() - 8.0).abs() < 1e-9);
        // Held into the near-limit band, it waits behind room.
        let mut hot = report(
            "mine",
            true,
            vec![window("5h", 80.0, Some(h)), window("7d", 0.0, Some(h))],
        );
        hot.reserve = 20;
        assert_eq!(choose(&[hot, other()], 0), Some(1));
    }

    #[test]
    fn near_limit_and_blocked_accounts_wait_behind_room() {
        let h = 3600;
        let reports = vec![
            report(
                "hot",
                true,
                vec![window("5h", 97.0, Some(h)), window("7d", 50.0, Some(h))],
            ),
            report("blocked", true, vec![window("7d", 100.0, Some(h))]),
            report("roomy", true, vec![window("7d", 5.0, Some(150 * h))]),
        ];
        assert_eq!(choose(&reports, 0), Some(2));
        assert_eq!(choose(&reports[..2], 0), Some(0));
    }

    #[test]
    fn model_scoped_caps_only_limit_their_model() {
        let h = 3600;
        let reports = vec![
            report(
                "fable-spent",
                true,
                vec![
                    window("7d", 60.0, Some(10 * h)),
                    window("7d fable", 100.0, Some(10 * h)),
                ],
            ),
            report("steady", true, vec![window("7d", 20.0, Some(140 * h))]),
        ];
        assert_eq!(choose_for(&reports, 0, Some("claude-fable-5-1")), Some(1));
        assert_eq!(choose_for(&reports, 0, Some("claude-sonnet-5-5")), Some(0));
        assert_eq!(choose(&reports, 0), Some(1));
    }

    #[test]
    fn rebalance_moves_only_for_a_clear_gain() {
        let h = 3600;
        let acct = |label: &str, used: f64, reset_h: i64| {
            report(label, true, vec![window("7d", used, Some(reset_h * h))])
        };
        let none = |_: &str| false;
        // 51% over 142h (0.36%/h) against 88% over 125h (0.70%/h): move.
        let reports = vec![acct("busy", 49.0, 142), acct("idle", 12.0, 125)];
        let m = rebalance_target(&reports, 0, None, "busy", none).unwrap();
        assert_eq!((m.to.as_str(), m.urgent), ("idle", false));
        assert!(m.why.contains("0.36"));
        // Already on the best account, or the best is ruled out: stay.
        assert!(rebalance_target(&reports, 0, None, "idle", none).is_none());
        assert!(rebalance_target(&reports, 0, None, "busy", |l| l == "idle").is_none());
        // 0.60 against 0.70: not worth losing the cache.
        let close = vec![acct("a", 0.0, 168), acct("b", 12.0, 125)];
        assert!(rebalance_target(&close, 0, None, "a", none).is_none());
        // Nowhere better to go, or the current account is unknown: stay.
        let full = vec![acct("x", 96.0, 100), acct("y", 97.0, 100)];
        assert!(rebalance_target(&full, 0, None, "x", none).is_none());
        assert!(rebalance_target(&reports, 0, None, "gone", none).is_none());
    }

    #[test]
    fn rebalance_leaves_a_limit_and_spares_a_busy_5h_window() {
        let h = 3600;
        let none = |_: &str| false;
        let with_5h = |label: &str, five: f64, week: f64| {
            report(
                label,
                true,
                vec![
                    window("5h", five, Some(h)),
                    window("7d", week, Some(140 * h)),
                ],
            )
        };
        // Near a limit: move to an account with room, at once.
        let hot = vec![with_5h("hot", 96.0, 20.0), with_5h("cool", 10.0, 30.0)];
        let m = rebalance_target(&hot, 0, None, "hot", none).unwrap();
        assert_eq!((m.to.as_str(), m.urgent), ("cool", true));
        // At a limit, every other account at 95-99%: still better than certain refusal.
        let stuck = vec![with_5h("stuck", 10.0, 100.0), with_5h("tight", 10.0, 97.0)];
        let m = rebalance_target(&stuck, 0, None, "stuck", none).unwrap();
        assert_eq!(
            (m.to.as_str(), m.why.as_str()),
            ("tight", "is at its limit")
        );
        // A much better weekly rate, but its 5-hour window is 75% full: stay.
        let busy = vec![with_5h("here", 10.0, 60.0), with_5h("there", 75.0, 5.0)];
        assert!(rebalance_target(&busy, 0, None, "here", none).is_none());
    }

    #[test]
    fn stamps_expire() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("acct.refused");
        assert!(!stamped_within_at(&path, 100, 60));
        stamp_at(&path, 100);
        assert!(stamped_within_at(&path, 159, 60));
        assert!(!stamped_within_at(&path, 160, 60));
    }

    #[test]
    fn model_arg_reads_both_spellings() {
        assert_eq!(
            model_arg(&["-p", "--model", "opus"]).as_deref(),
            Some("opus")
        );
        assert_eq!(
            model_arg(&["--model=claude-haiku-5-5"]).as_deref(),
            Some("claude-haiku-5-5")
        );
        assert_eq!(model_arg(&["--effort", "high"]), None);
        assert_eq!(model_arg::<&str>(&[]), None);
    }

    #[test]
    fn refresh_only_when_stale_and_refreshable() {
        let now = 10_000;
        let soon = json!({"claudeAiOauth": {"accessToken": "a", "refreshToken": "r",
                          "expiresAt": (now + 60) * 1000}});
        let later = json!({"claudeAiOauth": {"accessToken": "a", "refreshToken": "r",
                           "expiresAt": (now + 3600) * 1000}});
        let no_rt = json!({"claudeAiOauth": {"accessToken": "a", "refreshToken": "",
                           "expiresAt": 0}});
        assert!(needs_refresh(&soon, now));
        assert!(!needs_refresh(&later, now));
        assert!(!needs_refresh(&no_rt, now));
    }

    #[test]
    fn live_rate_limit_updates_the_cache_and_keeps_other_windows() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("acct.json");
        write_cache(
            &path,
            1,
            &json!({"seven_day": {"utilization": 10.0},
                    "limits": [{"kind": "weekly_scoped", "percent": 5,
                                "scope": {"model": {"display_name": "Fable"}}}]}),
        );
        let info = json!({"unifiedWindows": {
            "five_hour": {"utilization": 0.25, "resetsAt": 2000},
            "seven_day": {"utilization": 0.97, "resetsAt": 3000}}});
        record_rate_limit_at(&path, &info, 500);

        let (at, usage) = read_cache(&path).unwrap();
        assert_eq!(at, 500);
        let w = parse_usage(&usage);
        let get = |n: &str| w.iter().find(|w| w.name == n).unwrap();
        assert_eq!(get("5h").used, 25.0);
        assert_eq!(get("5h").resets_at, Some(2000));
        assert_eq!(get("7d").used, 97.0);
        assert_eq!(get("7d fable").used, 5.0);
    }

    #[test]
    fn short_duration_formats() {
        assert_eq!(short_duration(40 * 60), "40m");
        assert_eq!(short_duration(2 * 3600 + 10 * 60), "2h10m");
        assert_eq!(short_duration(3 * 86_400 + 4 * 3600), "3d4h");
        assert_eq!(short_duration(30), "<1m");
        assert_eq!(short_duration(-5), "0m");
    }
}
