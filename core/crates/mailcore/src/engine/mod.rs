//! The engine owns the store, the credential vault and one worker per account.
//! The HTTP API and the macOS helper only talk to [`Engine`].

mod accounts;
mod actions;
mod demo;
pub(crate) mod detail;
mod extras;
mod imap_worker;
mod keys;
mod newmail;
mod ops;
mod pop_worker;
mod send;

pub use accounts::{AccountTest, NewAccount, TestReport};
pub use actions::{Action, ActionResult};
pub use keys::KeyImport;
pub use send::UnsubscribeResult;

use crate::error::{Error, Result};
use crate::events::{Event, EventBus};
use crate::imap::{ImapAuth, ImapConn};
use crate::model::*;
use crate::oauth;
use crate::paths::Paths;
use crate::secrets::{self, Secrets};
use crate::store::{Db, Snapshot};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct Worker {
    wake: Arc<Notify>,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

pub(crate) struct PendingOAuth {
    pub provider: String,
    pub verifier: String,
    pub redirect_uri: String,
    pub created: Instant,
    pub tokens: Option<oauth::Tokens>,
}

pub(crate) struct UndoEntry {
    pub ops: Vec<(String, i64)>,
    pub snapshots: Vec<Snapshot>,
    /// Flag-like actions are undone by pushing the old values; moves by cancelling their ops.
    pub inverse: bool,
    pub created: Instant,
}

type ConnSlot = Arc<tokio::sync::Mutex<Option<(ImapConn, Instant)>>>;

pub struct Inner {
    pub paths: Paths,
    pub db: Db,
    pub secrets: Secrets,
    pub events: EventBus,
    workers: Mutex<HashMap<String, Worker>>,
    status: Mutex<HashMap<String, AccountStatus>>,
    pub(crate) oauth_pending: Mutex<HashMap<String, PendingOAuth>>,
    token_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    ondemand: Mutex<HashMap<String, ConnSlot>>,
    pub(crate) undo: Mutex<HashMap<String, UndoEntry>>,
    pub(crate) folder_requests: Mutex<HashMap<String, HashSet<FolderId>>>,
    outbox_wake: Notify,
    pub api_port: AtomicU16,
    shutdown: CancellationToken,
    pub(crate) http: reqwest::Client,
    /// AI features: batch jobs, the new-mail analysis queue, running loops.
    pub(crate) ai: crate::ai::AiState,
}

/// Cheap to clone handle to the engine.
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

impl std::ops::Deref for Engine {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

pub(crate) enum Cred {
    Password(String),
    OAuth(String),
}

impl Cred {
    pub fn imap(&self) -> ImapAuth {
        match self {
            Cred::Password(p) => ImapAuth::Password(p.clone()),
            Cred::OAuth(t) => ImapAuth::OAuth2(t.clone()),
        }
    }
}

impl Engine {
    pub fn open(data_dir: impl AsRef<std::path::Path>) -> Result<Engine> {
        crate::net::install_crypto_provider();
        let paths = Paths::new(data_dir)?;
        let db = Db::open(paths.db())?;
        let secrets = Secrets::open(&paths)?;
        Ok(Engine(Arc::new(Inner {
            paths,
            db,
            secrets,
            events: EventBus::default(),
            workers: Mutex::new(HashMap::new()),
            status: Mutex::new(HashMap::new()),
            oauth_pending: Mutex::new(HashMap::new()),
            token_locks: Mutex::new(HashMap::new()),
            ondemand: Mutex::new(HashMap::new()),
            undo: Mutex::new(HashMap::new()),
            folder_requests: Mutex::new(HashMap::new()),
            outbox_wake: Notify::new(),
            api_port: AtomicU16::new(0),
            shutdown: CancellationToken::new(),
            http: crate::forward::http_client(),
            ai: crate::ai::AiState::new(),
        })))
    }

    /// Starts account workers and the background schedulers. Must run inside a tokio runtime.
    pub fn start(&self) {
        for acc in self.db.accounts().unwrap_or_default() {
            self.spawn_worker(&acc);
        }
        let e = self.clone();
        tokio::spawn(async move { e.outbox_loop().await });
        let e = self.clone();
        tokio::spawn(async move { e.housekeeping_loop().await });
        crate::ai::start(self);
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let workers: Vec<Worker> = self.workers.lock().drain().map(|(_, w)| w).collect();
        for w in workers {
            w.cancel.cancel();
            for t in w.tasks {
                let _ = tokio::time::timeout(Duration::from_secs(3), t).await;
            }
        }
        let slots: Vec<ConnSlot> = self.ondemand.lock().drain().map(|(_, s)| s).collect();
        for s in slots {
            if let Some((c, _)) = s.lock().await.take() {
                c.logout().await;
            }
        }
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    // ---- workers -------------------------------------------------------------------------

    pub(crate) fn spawn_worker(&self, acc: &Account) {
        self.stop_worker(&acc.id);
        if !acc.enabled {
            self.set_status(&acc.id, SyncState::Disabled, None);
            return;
        }
        if acc.provider.as_deref() == Some("demo") {
            self.set_status(&acc.id, SyncState::Idle, None);
            return;
        }
        let wake = Arc::new(Notify::new());
        let cancel = self.shutdown.child_token();
        let mut tasks = Vec::new();
        match acc.kind {
            AccountKind::Imap => {
                let e = self.clone();
                let (id, w, c) = (acc.id.clone(), wake.clone(), cancel.clone());
                tasks.push(tokio::spawn(async move { imap_worker::run(e, id, w, c).await }));
                if acc.config.use_idle {
                    let e = self.clone();
                    let (id, w, c) = (acc.id.clone(), wake.clone(), cancel.clone());
                    tasks.push(tokio::spawn(async move { imap_worker::idle_loop(e, id, w, c).await }));
                }
            }
            AccountKind::Pop3 => {
                let e = self.clone();
                let (id, w, c) = (acc.id.clone(), wake.clone(), cancel.clone());
                tasks.push(tokio::spawn(async move { pop_worker::run(e, id, w, c).await }));
            }
            AccountKind::Graph | AccountKind::Jmap | AccountKind::Web => {
                self.set_status(&acc.id, SyncState::Idle, None);
            }
        }
        self.workers.lock().insert(acc.id.clone(), Worker { wake, cancel, tasks });
    }

    pub(crate) fn stop_worker(&self, account_id: &str) {
        if let Some(w) = self.workers.lock().remove(account_id) {
            w.cancel.cancel();
        }
    }

    /// Asks the worker of an account to sync now (or all accounts with `None`).
    pub fn wake(&self, account_id: Option<&str>) {
        let workers = self.workers.lock();
        match account_id {
            Some(id) => {
                if let Some(w) = workers.get(id) {
                    w.wake.notify_one();
                }
            }
            None => workers.values().for_each(|w| w.wake.notify_one()),
        }
    }

    /// Syncs one folder as soon as possible (the user opened it).
    pub fn request_folder_sync(&self, folder_id: FolderId) -> Result<()> {
        let f = self.db.folder(folder_id)?;
        if f.local {
            return Ok(());
        }
        self.folder_requests.lock().entry(f.account_id.clone()).or_default().insert(folder_id);
        self.wake(Some(&f.account_id));
        Ok(())
    }

    // ---- status & events -----------------------------------------------------------------

    pub(crate) fn set_status(&self, account_id: &str, state: SyncState, error: Option<String>) {
        let status = {
            let mut map = self.status.lock();
            let s = map.entry(account_id.to_string()).or_insert_with(|| AccountStatus {
                account_id: account_id.to_string(),
                ..Default::default()
            });
            s.state = state;
            s.error = error;
            if state == SyncState::Idle {
                s.last_sync = Some(crate::store::now());
            }
            s.clone()
        };
        self.events.emit(Event::AccountStatus { status });
    }

    pub fn statuses(&self) -> Vec<AccountStatus> {
        let counts: HashMap<String, i64> = self.db.unread_counts().unwrap_or_default().into_iter().collect();
        let map = self.status.lock();
        self.db
            .accounts()
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let mut s = map.get(&a.id).cloned().unwrap_or(AccountStatus { account_id: a.id.clone(), ..Default::default() });
                s.unread = counts.get(&a.id).copied().unwrap_or(0);
                s
            })
            .collect()
    }

    pub(crate) fn emit_counts(&self) {
        if let Ok(accounts) = self.db.unread_counts() {
            let total = accounts.iter().map(|(_, n)| n).sum();
            self.events.emit(Event::UnreadCounts { total, accounts });
        }
    }

    pub(crate) fn changed(&self, account_id: Option<&str>) {
        self.events.emit(Event::MessagesChanged { account_id: account_id.map(String::from) });
        self.emit_counts();
    }

    // ---- credentials ---------------------------------------------------------------------

    fn token_lock(&self, account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.token_locks.lock().entry(account_id.to_string()).or_default().clone()
    }

    /// A valid OAuth access token, refreshed when it expires within a minute.
    pub(crate) async fn access_token(&self, acc: &Account) -> Result<String> {
        let lock = self.token_lock(&acc.id);
        let _guard = lock.lock().await;
        let raw = self
            .secrets
            .get(&secrets::oauth_key(&acc.id))
            .ok_or_else(|| Error::Auth("缺少 OAuth 令牌，请重新登录".into()))?;
        let mut tokens: oauth::Tokens = serde_json::from_str(&raw)?;
        if tokens.expires_at - 60 > crate::store::now() {
            return Ok(tokens.access_token);
        }
        let refresh = tokens.refresh_token.clone().ok_or_else(|| Error::Auth("OAuth 令牌已过期，请重新登录".into()))?;
        let provider_id = acc.config.oauth_provider.clone().unwrap_or(tokens.provider.clone());
        let provider = oauth::provider(&provider_id, &self.db.settings()?.oauth)?;
        let fresh = oauth::refresh(&provider, &refresh).await.map_err(|e| match e {
            Error::OAuth(m) if m.contains("invalid_grant") => Error::Auth(format!("授权已失效，请重新登录 ({m})")),
            other => other,
        })?;
        tokens.access_token = fresh.access_token;
        tokens.expires_at = fresh.expires_at;
        if fresh.refresh_token.is_some() {
            tokens.refresh_token = fresh.refresh_token;
        }
        self.secrets.set(&secrets::oauth_key(&acc.id), &serde_json::to_string(&tokens)?)?;
        Ok(tokens.access_token)
    }

    pub(crate) async fn credentials(&self, acc: &Account) -> Result<Cred> {
        match acc.config.auth {
            AuthKind::Oauth2 => Ok(Cred::OAuth(self.access_token(acc).await?)),
            AuthKind::Password => self
                .secrets
                .get(&secrets::password_key(&acc.id))
                .map(Cred::Password)
                .ok_or_else(|| Error::Auth("缺少密码 / 授权码".into())),
        }
    }

    pub(crate) async fn smtp_auth(&self, acc: &Account) -> Result<crate::smtp::SmtpAuth> {
        use crate::smtp::SmtpAuth;
        match acc.config.auth {
            AuthKind::Oauth2 => Ok(SmtpAuth::OAuth2(self.access_token(acc).await?)),
            AuthKind::Password => {
                let p = self
                    .secrets
                    .get(&secrets::smtp_password_key(&acc.id))
                    .or_else(|| self.secrets.get(&secrets::password_key(&acc.id)));
                Ok(p.map(SmtpAuth::Password).unwrap_or(SmtpAuth::None))
            }
        }
    }

    pub(crate) fn login_name(acc: &Account, server: &ServerConfig) -> String {
        server.username.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| acc.email.clone())
    }

    pub(crate) async fn imap_connect(&self, acc: &Account) -> Result<ImapConn> {
        let server = acc.config.imap.as_ref().ok_or_else(|| Error::Invalid("未配置 IMAP 服务器".into()))?;
        let cred = self.credentials(acc).await?;
        ImapConn::connect(server, &Self::login_name(acc, server), &cred.imap(), acc.config.send_id).await
    }

    /// Runs `f` on a cached short-lived IMAP connection (body downloads, server search).
    pub(crate) async fn with_ondemand<T, F>(&self, acc: &Account, f: F) -> Result<T>
    where
        F: for<'c> FnOnce(&'c mut ImapConn) -> futures::future::BoxFuture<'c, Result<T>>,
    {
        let slot = self.ondemand.lock().entry(acc.id.clone()).or_default().clone();
        let mut guard = slot.lock().await;
        let mut conn = match guard.take() {
            Some((mut c, last)) if last.elapsed() < Duration::from_secs(120) => {
                if c.noop().await.is_ok() { c } else { self.imap_connect(acc).await? }
            }
            Some((c, _)) => {
                c.logout().await;
                self.imap_connect(acc).await?
            }
            None => self.imap_connect(acc).await?,
        };
        let out = f(&mut conn).await;
        if out.is_ok() {
            *guard = Some((conn, Instant::now()));
        }
        out
    }

    // ---- background loops ----------------------------------------------------------------

    async fn housekeeping_loop(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        let mut last_cleanup = Instant::now();
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tick.tick() => {}
            }
            let now = crate::store::now();
            if let Ok(ids) = self.db.wake_snoozed(now) {
                if !ids.is_empty() {
                    for id in &ids {
                        self.events.emit(Event::Snoozed { message_id: *id });
                        if let Ok(m) = self.db.message(*id) {
                            self.notify_message(&m.summary, None).await;
                        }
                    }
                    self.changed(None);
                }
            }
            // Smart reminders and scheduled AI loops (checked once a minute).
            crate::ai::tick(self);
            self.undo.lock().retain(|_, u| u.created.elapsed() < Duration::from_secs(60));
            self.oauth_pending.lock().retain(|_, p| p.created.elapsed() < Duration::from_secs(1800));
            let idle: Vec<ConnSlot> = self.ondemand.lock().values().cloned().collect();
            for slot in idle {
                if let Ok(mut g) = slot.try_lock() {
                    if g.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(120)) {
                        if let Some((c, _)) = g.take() {
                            c.logout().await;
                        }
                    }
                }
            }
            let _ = self.db.purge_outbox(now - 7 * 86_400);
            if last_cleanup.elapsed() > Duration::from_secs(24 * 3600) {
                last_cleanup = Instant::now();
                if let Err(e) = self.cleanup() {
                    tracing::warn!(error = %e, "cache cleanup failed");
                }
            }
        }
    }

    pub fn settings(&self) -> Settings {
        self.db.settings().unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.db.save_settings(s)?;
        self.events.emit(Event::SettingsChanged);
        Ok(())
    }

    pub(crate) fn port(&self) -> u16 {
        self.api_port.load(Ordering::Relaxed)
    }
}

/// Whether `now` (local time) falls into the quiet hours window.
pub fn in_quiet_hours(q: &QuietHours, now: chrono::DateTime<chrono::Local>) -> bool {
    if !q.enabled {
        return false;
    }
    let parse = |s: &str| {
        let (h, m) = s.split_once(':')?;
        Some(h.trim().parse::<u32>().ok()? * 60 + m.trim().parse::<u32>().ok()?)
    };
    let (Some(start), Some(end)) = (parse(&q.start), parse(&q.end)) else { return false };
    use chrono::Timelike;
    let cur = now.hour() * 60 + now.minute();
    if start <= end { cur >= start && cur < end } else { cur >= start || cur < end }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn quiet_hours_across_midnight() {
        let q = QuietHours { enabled: true, start: "22:00".into(), end: "08:00".into() };
        let at = |h, m| chrono::Local.with_ymd_and_hms(2026, 10, 23, h, m, 0).unwrap();
        assert!(in_quiet_hours(&q, at(23, 0)));
        assert!(in_quiet_hours(&q, at(7, 59)));
        assert!(!in_quiet_hours(&q, at(8, 0)));
        assert!(!in_quiet_hours(&q, at(12, 0)));
        let day = QuietHours { enabled: true, start: "12:00".into(), end: "13:30".into() };
        assert!(in_quiet_hours(&day, at(13, 0)));
        assert!(!in_quiet_hours(&QuietHours::default(), at(23, 0)));
    }
}
