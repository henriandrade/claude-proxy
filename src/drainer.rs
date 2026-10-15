//! The process behind a run (`claude-proxy __drain <id>`): keeps one stream-json
//! `claude` warm for the run and exits once idle for `CLAUDE_PROXY_IDLE_SECS`.

use std::collections::{HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::asks;
use crate::quota::{self, now_secs};
use crate::runs::{
    classify, enqueue_first, excerpt, first_account, inbox_names, load, lock_file, marker,
    next_account, notify_parent, observe, parse_pool, queued, request_drainer, run_dir,
    session_len, signal_group, take_inbox, turn_command, Meta, Reason, Signals, State,
};

const POLL: Duration = Duration::from_millis(300);

fn idle_timeout() -> Duration {
    let secs = std::env::var("CLAUDE_PROXY_IDLE_SECS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(300);
    Duration::from_secs(secs)
}

pub fn drain(id: &str) -> Result<(), String> {
    let dir = run_dir(id);
    let lock = lock_file(&dir).map_err(|e| e.to_string())?;
    // Blocking, never `try_lock`: `alive()` probes take this lock briefly, and a
    // drainer giving up on contention could leave its message queued forever.
    lock.lock().map_err(|e| e.to_string())?;
    let _ = fs::remove_file(dir.join("pending"));
    let _ = fs::remove_file(dir.join("release"));
    let events = crate::paths::append(&dir.join("events.jsonl"))
        .map_err(|e| format!("could not open the transcript: {e}"))?;
    let mut d = Drainer {
        meta: load(id)?,
        dir,
        events,
        proc: None,
        unechoed: VecDeque::new(),
        busy: false,
        signals: Signals::default(),
        tried: Vec::new(),
        session_at_start: 0,
        idle_since: Instant::now(),
        idle: idle_timeout(),
        running: 0,
        wake_due: None,
    };
    loop {
        match d.step() {
            Flow::Continue => {}
            Flow::Idle => {
                d.close();
                // Re-check after unlocking: a message queued while finishing
                // would otherwise wait for the next `send`.
                let _ = lock.unlock();
                if queued(&d.dir) == 0 || lock.lock().is_err() {
                    return Ok(());
                }
                d.meta = load(id)?;
                d.idle_since = Instant::now();
            }
            Flow::Stop => {
                let waiting: HashSet<String> = inbox_names(&d.dir).into_iter().collect();
                d.close();
                let _ = lock.unlock();
                // A message sent while stopping found this drainer alive and
                // started nothing; start the drainer it expected.
                if inbox_names(&d.dir).iter().any(|n| !waiting.contains(n))
                    && !d.dir.join("stop").exists()
                {
                    request_drainer(id)?;
                }
                return Ok(());
            }
        }
    }
}

enum Flow {
    Continue,
    Idle,
    /// Failed, killed, or released; queued messages wait for `send`.
    Stop,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Proc {
    fn spawn(dir: &Path, meta: &Meta) -> Result<Proc, String> {
        let mut child = turn_command(dir, meta)?.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "`claude` is not on PATH".to_string()
            } else {
                format!("could not start claude: {e}")
            }
        })?;
        let stdin = child.stdin.take().ok_or("claude has no stdin")?;
        let stdout = child.stdout.take().ok_or("claude has no stdout")?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Proc {
            child,
            stdin,
            lines,
        })
    }

    fn write(&mut self, text: &str) -> std::io::Result<()> {
        let message = json!({"type": "user", "message": {"role": "user", "content": text}});
        self.stdin.write_all(format!("{message}\n").as_bytes())?;
        self.stdin.flush()
    }

    fn end(self, force: bool) -> Option<i32> {
        let Proc {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        if force {
            signal_group(child.id(), false);
        }
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return status.code();
            }
            if started.elapsed() > Duration::from_secs(5) {
                signal_group(child.id(), true);
                return child.wait().ok().and_then(|s| s.code());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

struct Drainer {
    dir: PathBuf,
    meta: Meta,
    events: File,
    proc: Option<Proc>,
    /// Written to Claude but not yet echoed back (`--replay-user-messages`):
    /// not taken into a turn.
    unechoed: VecDeque<String>,
    busy: bool,
    signals: Signals,
    tried: Vec<String>,
    session_at_start: u64,
    idle_since: Instant,
    idle: Duration,
    running: u32,
    /// Since a background task ended while Claude was idle; it should wake up.
    wake_due: Option<Instant>,
}

/// How long to wait for Claude to wake up on a finished task.
const WAKE_GRACE: Duration = Duration::from_secs(120);

impl Drainer {
    /// The drainer's stderr is the run's stderr.log.
    fn save(&mut self) {
        if let Err(e) = crate::runs::save(&self.dir, &mut self.meta) {
            eprintln!("claude-proxy: {e}");
        }
    }

    fn step(&mut self) -> Flow {
        if self.dir.join("stop").exists() {
            return self.stop();
        }
        if !self.busy && self.dir.join("release").exists() {
            let _ = fs::remove_file(self.dir.join("release"));
            return Flow::Stop;
        }
        if queued(&self.dir) > 0 {
            if let Some(flow) = self.intake() {
                return flow;
            }
        }
        if self.wake_due.is_some_and(|t| t.elapsed() > WAKE_GRACE) {
            self.wake_due = None;
            self.sync_background();
        }
        // Kept while Claude has background tasks: it wakes when they end.
        let idle_out = self.idle_since.elapsed() >= self.idle && self.meta.background == 0;
        if !self.busy && (self.proc.is_none() || idle_out) {
            return Flow::Idle;
        }
        let Some(proc) = self.proc.as_mut() else {
            std::thread::sleep(POLL);
            return Flow::Continue;
        };
        match proc.lines.recv_timeout(POLL) {
            Ok(line) => self.on_line(&line),
            Err(RecvTimeoutError::Timeout) => Flow::Continue,
            Err(RecvTimeoutError::Disconnected) => self.on_exit(),
        }
    }

    fn intake(&mut self) -> Option<Flow> {
        let switching = self.dir.join("account").exists();
        if self.busy && (switching || self.proc.is_none()) {
            return None;
        }
        if switching {
            self.switch_account();
        }
        let during_turn = self.busy;
        let before = self.meta.state;
        if !during_turn {
            // Marked working before the inbox is emptied, so a waiter sees the
            // message in one place or the other.
            self.meta.state = State::Working;
            self.save();
        }
        let _ = fs::remove_file(self.dir.join("pending"));
        let messages = take_inbox(&self.dir);
        if messages.is_empty() {
            if !during_turn {
                self.meta.state = before;
                self.save();
            }
            return None;
        }
        let text = messages.join("\n\n");
        marker(
            &self.dir,
            json!({"event": "message", "text": text, "during_turn": during_turn}),
        );
        if !during_turn {
            self.start_turn();
        }
        match self.write(&text) {
            Ok(()) => None,
            Err(error) => Some(self.cannot_start(error)),
        }
    }

    fn switch_account(&mut self) {
        let Ok(spec) = fs::read_to_string(self.dir.join("account")) else {
            return;
        };
        let _ = fs::remove_file(self.dir.join("account"));
        match parse_pool(spec.trim()).and_then(|pool| Ok((first_account(&pool)?, pool))) {
            Ok((account, pool)) => {
                if account != self.meta.account {
                    self.close();
                }
                self.meta.account = account;
                self.meta.pool = pool;
                self.meta.moved = None;
            }
            Err(e) => marker(
                &self.dir,
                json!({"event": "note", "text": format!("account switch ignored: {e}")}),
            ),
        }
    }

    /// A wake-up Claude owes for an ended task counts as background work.
    fn sync_background(&mut self) {
        let background = self.running + u32::from(self.wake_due.is_some());
        if background != self.meta.background {
            self.meta.background = background;
            self.save();
        }
    }

    fn start_turn(&mut self) {
        self.wake_due = None;
        self.meta.background = self.running;
        self.busy = true;
        self.signals = Signals::default();
        self.tried = vec![self.meta.account.clone()];
        self.begin_attempt();
    }

    /// One account's attempt at the current turn.
    fn begin_attempt(&mut self) {
        self.session_at_start = session_len(&self.meta);
        self.meta.turns += 1;
        self.meta.state = State::Working;
        self.meta.turn_started_at = Some(now_secs());
        self.meta.activity = None;
        self.meta.summary = None;
        self.meta.needs_action = None;
        self.save();
        marker(
            &self.dir,
            json!({"event": "turn_start", "turn": self.meta.turns, "account": self.meta.account}),
        );
    }

    fn write(&mut self, text: &str) -> Result<(), String> {
        self.unechoed.push_back(text.to_string());
        if self.proc.is_none() {
            let proc = Proc::spawn(&self.dir, &self.meta)?;
            self.meta.turn_pid = Some(proc.child.id());
            self.save();
            self.proc = Some(proc);
        }
        if let Some(proc) = self.proc.as_mut() {
            // A failed write means Claude exited; stdout's end says why.
            let _ = proc.write(text);
        }
        Ok(())
    }

    fn on_line(&mut self, line: &str) -> Flow {
        // One write per line: `__permit` appends to the same file.
        let _ = self.events.write_all(format!("{line}\n").as_bytes());
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            if line.trim_start().starts_with("Not logged in") {
                self.signals.not_logged_in = true;
            }
            return Flow::Continue;
        };
        if v["type"] == "user" && v["isReplay"] == true {
            self.unechoed.pop_front();
            return Flow::Continue;
        }
        if v["subtype"] == "background_tasks_changed" {
            let running = v["tasks"].as_array().map_or(0, |t| t.len() as u32);
            if running < self.running && !self.busy {
                self.wake_due = Some(Instant::now());
            }
            self.running = running;
            self.sync_background();
        }
        if !self.busy && v["type"] == "assistant" {
            // A background task ended and Claude took it up on its own.
            marker(
                &self.dir,
                json!({"event": "note", "text": "woken by a background task"}),
            );
            self.start_turn();
        }
        if observe(&mut self.meta, &v, &mut self.signals) {
            self.save();
        }
        if v["type"] == "result" {
            return self.on_result();
        }
        Flow::Continue
    }

    fn on_result(&mut self) -> Flow {
        if self.signals.result_error {
            return self.on_failure();
        }
        if self.unechoed.is_empty() {
            self.busy = false;
            if self.meta.background == 0 {
                // Before the run reads as done, so a waiter on the parent
                // already sees the message queued.
                let result = self.meta.last_result.clone().unwrap_or_default();
                notify_parent(
                    &self.meta,
                    &format!(
                        "finished:\n\n{}\n\nWhole answer: `claude-proxy result {}`.",
                        excerpt(&result, 2000),
                        self.meta.id
                    ),
                );
            }
            self.meta.state = State::Idle;
            self.meta.activity = None;
            self.save();
            self.idle_since = Instant::now();
        } else {
            // A message that arrived as the turn ended starts the next one.
            self.start_turn();
        }
        Flow::Continue
    }

    fn on_exit(&mut self) -> Flow {
        let code = self.proc.take().and_then(|p| p.end(false));
        asks::clear(&self.dir);
        self.forget_background();
        self.meta.turn_pid = None;
        self.meta.last_exit = code;
        if !self.busy {
            self.save();
            return Flow::Continue;
        }
        let killed = self.dir.join("stop").exists();
        marker(
            &self.dir,
            json!({"event": "turn_end", "turn": self.meta.turns, "exit": code, "killed": killed}),
        );
        if killed {
            return self.stop();
        }
        self.on_failure()
    }

    fn on_failure(&mut self) -> Flow {
        self.close();
        if let Some(reason) = classify(&self.signals) {
            if self.failover(reason) {
                return Flow::Continue;
            }
        }
        self.busy = false;
        let why = self.meta.last_result.clone().unwrap_or_default();
        notify_parent(
            &self.meta,
            &format!(
                "failed: {}\n\nSee `claude-proxy status {}`; `claude-proxy send {} \"…\"` resumes it.",
                excerpt(&why, 500),
                self.meta.id,
                self.meta.id
            ),
        );
        self.meta.state = State::Failed;
        self.meta.activity = None;
        self.requeue();
        self.save();
        Flow::Stop
    }

    /// Every profile shares the transcripts, so nothing is copied. Returns
    /// whether it moved.
    fn failover(&mut self, reason: Reason) -> bool {
        let from = self.meta.account.clone();
        if reason == Reason::Limit {
            quota::invalidate(&from);
        }
        let next = if self.dir.join("stop").exists() {
            None
        } else {
            next_account(&self.meta, &self.tried)
        };
        marker(
            &self.dir,
            json!({"event": "failover", "from": from, "to": next, "reason": reason.describe()}),
        );
        let Some(next) = next else {
            return false;
        };
        // Measured before switching: did the failed attempt reach the session?
        let reached = session_len(&self.meta) > self.session_at_start;
        self.meta.moved = Some(format!("from {from}, which {}", reason.describe()));
        self.meta.account = next.clone();
        self.tried.push(next);
        self.signals = Signals::default();
        let mut parts = Vec::new();
        if reached || self.unechoed.is_empty() {
            // The session already has it: ask for the rest, not a repeat.
            parts.push(format!(
                "(claude-proxy) Your previous turn stopped because the Claude account \
                 {from} {}. This conversation now continues on another account — pick up \
                 exactly where you left off.",
                reason.describe()
            ));
        }
        parts.extend(self.unechoed.drain(..));
        self.begin_attempt();
        match self.write(&parts.join("\n\n")) {
            Ok(()) => true,
            Err(error) => {
                self.note_error(&error);
                false
            }
        }
    }

    fn cannot_start(&mut self, error: String) -> Flow {
        self.note_error(&error);
        self.busy = false;
        self.meta.state = State::Failed;
        self.requeue();
        self.save();
        Flow::Stop
    }

    fn note_error(&mut self, error: &str) {
        marker(
            &self.dir,
            json!({"event": "turn_end", "turn": self.meta.turns, "exit": null, "error": error}),
        );
        self.meta.last_result = Some(error.to_string());
    }

    fn stop(&mut self) -> Flow {
        if let Some(proc) = self.proc.take() {
            let code = proc.end(true);
            if self.busy {
                marker(
                    &self.dir,
                    json!({"event": "turn_end", "turn": self.meta.turns, "exit": code, "killed": true}),
                );
            }
        }
        asks::clear(&self.dir);
        self.forget_background();
        if self.busy {
            self.meta.state = State::Killed;
        }
        self.busy = false;
        self.meta.activity = None;
        self.meta.turn_pid = None;
        self.requeue();
        self.save();
        Flow::Stop
    }

    fn forget_background(&mut self) {
        self.running = 0;
        self.wake_due = None;
        self.meta.background = 0;
    }

    /// Ahead of anything queued since.
    fn requeue(&mut self) {
        for (i, text) in self.unechoed.drain(..).enumerate() {
            let _ = enqueue_first(&self.dir, &text, i);
        }
    }

    fn close(&mut self) {
        if let Some(proc) = self.proc.take() {
            proc.end(self.busy);
            asks::clear(&self.dir);
            self.forget_background();
            self.meta.turn_pid = None;
            self.save();
        }
    }
}
