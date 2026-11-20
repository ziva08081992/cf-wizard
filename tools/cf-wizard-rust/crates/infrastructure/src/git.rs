//! Read-only provider strategies. Every content request is pinned to a commit.
use crate::{canonical::key, source::relative_path};
use base64::{Engine, engine::general_purpose::STANDARD};
use cf_application::Cancellation;
use cf_application::profile;
use cf_domain::{Error, Provider, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::{Arc, Mutex},
    time::Duration,
};
use zeroize::Zeroizing;
#[cfg(test)]
#[path = "git_cold_inventory_perf.rs"]
pub(crate) mod cold_inventory_perf;
#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Connection {
    #[serde(default = "default_provider")]
    pub provider: String,
    pub server: String,
    pub repository: String,
    pub directory: String,
    pub branch: String,
    #[serde(default)]
    pub cache: String,
    #[serde(default = "default_interval")]
    pub interval: u32,
}
fn default_provider() -> String {
    "gitea".into()
}
fn default_interval() -> u32 {
    60
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            server: String::new(),
            repository: String::new(),
            directory: "config".into(),
            branch: String::new(),
            cache: String::new(),
            interval: 60,
        }
    }
}
impl Connection {
    pub fn provider(&self) -> Result<Provider> {
        match self.provider.as_str() {
            "gitea" => Ok(Provider::Gitea),
            "github" => Ok(Provider::Github),
            _ => Err(invalid("Неизвестный API Git")),
        }
    }
    pub fn checked(&self) -> Result<Self> {
        let mut c = self.clone();
        let p = c.provider()?;
        c.server = canonical_server(&c.server, p)?;
        c.repository = relative_path(c.repository.trim())?.into();
        if c.repository.split('/').count() != 2 {
            return Err(invalid(
                "Укажите репозиторий в формате владелец/репозиторий",
            ));
        }
        c.directory = relative_path(c.directory.trim())?.into();
        if c.cache.trim().is_empty() {
            return Err(invalid("Выберите папку кэша Git"));
        }
        validate_branch(&c.branch)?;
        if c.interval != 0 && !(30..=86400).contains(&c.interval) {
            return Err(invalid("Интервал проверки SHA: 0 или 30–86400 секунд"));
        }
        Ok(c)
    }
    pub fn repo_key(&self) -> String {
        if self.provider == "github" {
            key(&["github", &self.server, &self.repository, &self.directory])
        } else {
            key(&[&self.server, &self.repository, &self.directory])
        }
    }
    pub fn source_key(&self) -> String {
        format!("git:{}", key(&[&self.repo_key(), &self.branch]))
    }
}
pub fn canonical_server(value: &str, provider: Provider) -> Result<String> {
    let s = value.trim().trim_end_matches('/');
    let parsed = url::Url::parse(s).map_err(|_| invalid("Укажите HTTPS-адрес сервера"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || s.contains('\\')
        || s.chars().any(char::is_whitespace)
        || s.contains('@')
    {
        return Err(invalid(
            "Нужен HTTPS-адрес без пароля, параметров и пробелов",
        ));
    }
    // Preserve explicit port/path in Gitea identity, including legacy :443.
    let (_, tail) = s
        .split_once("://")
        .ok_or_else(|| invalid("Неверный адрес"))?;
    let (authority, path) = tail.split_once('/').map_or((tail, ""), |(a, p)| (a, p));
    let result = format!(
        "https://{}{}",
        authority.to_lowercase(),
        if path.is_empty() {
            String::new()
        } else {
            format!("/{path}")
        }
    );
    if provider == Provider::Github {
        if result != "https://github.com" && result != "https://api.github.com" {
            return Err(invalid("Поддерживается GitHub.com: https://github.com"));
        }
        return Ok("https://github.com".into());
    }
    Ok(result)
}
pub fn validate_branch(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || value.contains("@{")
        || value.ends_with('.')
        || value
            .split('/')
            .any(|p| p.is_empty() || p.starts_with('.') || p.ends_with(".lock"))
        || value.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
    {
        return Err(invalid("Неверное имя ветки"));
    }
    Ok(())
}
fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}
fn malformed() -> Error {
    invalid("Некорректный или неполный ответ Git API")
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k).and_then(Value::as_str).ok_or_else(malformed)
}
pub fn oid(value: &str, github: bool) -> Result<String> {
    if !(value.len() == 40 || !github && value.len() == 64)
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(malformed());
    }
    Ok(value.into())
}
pub(super) fn file_read_limit(size: u64) -> Result<usize> {
    usize::try_from(size)
        .ok()
        .and_then(|size| size.checked_add(1))
        .ok_or_else(|| invalid("Размер файла Git превышает допустимый предел чтения"))
}
pub fn verify_blob(data: &[u8], id: &str, size: u64) -> Result<()> {
    profile::measure("hash.git_oid", "verification", data.len(), None, || {
        if data.len() as u64 != size {
            return Err(invalid("Размер файла Git не совпал с описью"));
        }
        let prefix = format!("blob {}\0", data.len());
        let digest = match id.len() {
            40 => {
                let mut h = sha1::Sha1::new();
                h.update(prefix);
                h.update(data);
                hex::encode(h.finalize())
            }
            64 => {
                let mut h = sha2::Sha256::new();
                h.update(prefix);
                h.update(data);
                hex::encode(h.finalize())
            }
            _ => return Err(malformed()),
        };
        if digest != id {
            return Err(invalid("Контрольная сумма файла Git не совпала с описью"));
        }
        if data.starts_with(b"version https://git-lfs.github.com/spec/v1") {
            return Err(invalid(
                "Файл находится в Git LFS. LFS в пилоте не поддерживается",
            ));
        }
        Ok(())
    })
}
fn quote(value: &str) -> String {
    value
        .split('/')
        .map(|p| {
            url::form_urlencoded::byte_serialize(p.as_bytes())
                .collect::<String>()
                .replace('+', "%20")
        })
        .collect::<Vec<_>>()
        .join("/")
}
#[derive(Debug)]
pub enum HttpFailure {
    Status(u16),
    ResponseTooLarge,
    Fatal(Error),
}

impl From<Error> for HttpFailure {
    fn from(error: Error) -> Self {
        Self::Fatal(error)
    }
}

impl HttpFailure {
    pub(super) fn into_domain_error(self) -> Error {
        match self {
            Self::Status(status) => status_error(status),
            Self::ResponseTooLarge => invalid("Ответ Git API превышает допустимый размер"),
            Self::Fatal(error) => error,
        }
    }
}

fn status_error(status: u16) -> Error {
    invalid(format!(
        "Git API: HTTP {status}. {}",
        match status {
            401 => "Проверьте токен.",
            403 | 429 => "Проверьте права и лимит API.",
            404 => "Проверьте репозиторий и права чтения.",
            300..=399 => "Перенаправления запрещены; укажите конечный HTTPS-адрес.",
            _ => "Запрос остановлен.",
        }
    ))
}

pub trait Http: Send + Sync {
    fn bytes(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>>;
    fn bytes_typed(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> std::result::Result<Vec<u8>, HttpFailure> {
        self.bytes(path, query, body, limit, raw, cancel)
            .map_err(HttpFailure::Fatal)
    }

    fn json(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        cancel: &Cancellation,
    ) -> Result<Value> {
        serde_json::from_slice(&self.bytes(path, query, body, 32 * 1024 * 1024, false, cancel)?)
            .map_err(|_| malformed())
    }
}
struct Https {
    client: reqwest::blocking::Client,
    base: String,
    origin: url::Url,
    token: Zeroizing<String>,
    github: bool,
}
impl Https {
    fn new(c: &Connection, token: Zeroizing<String>) -> Result<Self> {
        let github = c.provider == "github";
        let origin = if github {
            "https://api.github.com".into()
        } else {
            c.server.clone()
        };
        let base = format!(
            "{}/{}repos/{}",
            origin,
            if github { "" } else { "api/v1/" },
            quote(&c.repository)
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("CF-Wizard/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| invalid("Не удалось создать HTTPS-клиент"))?;
        Ok(Self {
            client,
            base,
            origin: url::Url::parse(&origin).map_err(|_| malformed())?,
            token,
            github,
        })
    }
}
impl Http for Https {
    fn bytes(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>> {
        self.bytes_typed(path, query, body, limit, raw, cancel)
            .map_err(HttpFailure::into_domain_error)
    }

    fn bytes_typed(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> std::result::Result<Vec<u8>, HttpFailure> {
        profile::measured_bytes(None, "http.response_body", "provider", None, || {
            cancel.check()?;
            let read_limit = u64::try_from(limit)
                .ok()
                .and_then(|limit| limit.checked_add(1))
                .ok_or_else(|| invalid("Недопустимый предел размера ответа Git API"))?;
            let address = if path == "/graphql" && self.github {
                "https://api.github.com/graphql".into()
            } else {
                format!("{}/{}", self.base, path)
            };
            let url = url::Url::parse(&address).map_err(|_| malformed())?;
            if url.scheme() != self.origin.scheme()
                || url.host_str() != self.origin.host_str()
                || url.port_or_known_default() != self.origin.port_or_known_default()
            {
                return Err(invalid("Запрещена передача токена на другой сервер").into());
            }
            let mut request = if body.is_some() {
                self.client.post(url)
            } else {
                self.client.get(url)
            };
            let authorization = reqwest::header::HeaderValue::from_str(&format!(
                "{} {}",
                if self.github { "Bearer" } else { "token" },
                self.token.as_str()
            ))
            .map_err(|_| invalid("Недопустимый токен"))?;
            request = request
                .query(query)
                .header(reqwest::header::AUTHORIZATION, authorization);
            if self.github {
                request = request.header("X-GitHub-Api-Version", "2026-03-10").header(
                    "Accept",
                    if raw {
                        "application/vnd.github.raw+json"
                    } else {
                        "application/vnd.github+json"
                    },
                );
            }
            if let Some(value) = body {
                request = request.json(&value);
            }
            let mut response = request
                .send()
                .map_err(|_| invalid("Ошибка HTTPS: проверьте адрес, сеть и сертификат сервера"))?;
            cancel.check()?;
            if !response.status().is_success() {
                return Err(HttpFailure::Status(response.status().as_u16()));
            }
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(HttpFailure::ResponseTooLarge);
            }
            let mut data = vec![];
            response
                .by_ref()
                .take(read_limit)
                .read_to_end(&mut data)
                .map_err(|_| invalid("Ошибка чтения Git API"))?;
            cancel.check()?;
            if data.len() > limit {
                return Err(HttpFailure::ResponseTooLarge);
            }
            Ok(data)
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Tree,
    Blob,
    Unsupported,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub oid: String,
    pub size: u64,
    pub kind: EntryKind,
    pub mode: String,
}

/// The provider collects rows and the source separately verifies their tree
/// structure. Unavailable recursive proofs permit a direct tree walk.
#[derive(Debug)]
pub enum RecursiveListing<T> {
    Complete(T),
    Unsupported,
    ProofIncomplete(RecursiveFallback),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecursiveFallback {
    InvalidResponse,
    InconsistentPages,
    PageLimit,
    RowLimit,
    BodyLimit,
    InvalidPath,
    PathLimit,
    DepthLimit,
    InvalidTree,
}

impl RecursiveFallback {
    pub fn metric(self) -> &'static str {
        match self {
            Self::InvalidResponse => "provider.tree_recursive_fallback_response",
            Self::InconsistentPages => "provider.tree_recursive_fallback_pages",
            Self::PageLimit => "provider.tree_recursive_fallback_page_limit",
            Self::RowLimit => "provider.tree_recursive_fallback_row_limit",
            Self::BodyLimit => "provider.tree_recursive_fallback_body_limit",
            Self::InvalidPath => "provider.tree_recursive_fallback_path",
            Self::PathLimit => "provider.tree_recursive_fallback_path_limit",
            Self::DepthLimit => "provider.tree_recursive_fallback_depth_limit",
            Self::InvalidTree => "provider.tree_recursive_fallback_proof",
        }
    }
}

#[derive(Debug)]
pub struct RecursiveRow {
    pub path: String,
    pub entry: Entry,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub oid: String,
    pub message: String,
    pub date: String,
}
pub trait GitProvider: Send + Sync {
    fn branches(&self, cancel: &Cancellation) -> Result<Vec<String>>;
    fn head(&self, branch: &str, cancel: &Cancellation) -> Result<String>;
    fn root(&self, revision: &str, cancel: &Cancellation) -> Result<String>;
    fn tree(&self, id: &str, cancel: &Cancellation) -> Result<Vec<Entry>>;
    fn recursive_tree(
        &self,
        _id: &str,
        cancel: &Cancellation,
    ) -> Result<RecursiveListing<Vec<RecursiveRow>>> {
        cancel.check()?;
        Ok(RecursiveListing::Unsupported)
    }
    fn file(
        &self,
        revision: &str,
        path: &str,
        entry: &Entry,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>>;
    fn files(
        &self,
        revision: &str,
        files: &[(String, Entry)],
        cancel: &Cancellation,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        files
            .iter()
            .map(|(path, e)| Ok((path.clone(), self.file(revision, path, e, cancel)?)))
            .collect()
    }
    fn commits(
        &self,
        revision: &str,
        since: Option<&str>,
        until: &str,
        cancel: &Cancellation,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<Commit>>;
    fn changed(&self, id: &str, directory: &str, cancel: &Cancellation) -> Result<Vec<String>>;
}
pub struct Gitea {
    http: Arc<dyn Http>,
    trees: Mutex<BTreeMap<String, Vec<Entry>>>,
}
pub struct Github {
    http: Arc<dyn Http>,
    owner: String,
    repo: String,
    trees: Mutex<BTreeMap<String, Vec<Entry>>>,
    commits: Mutex<BTreeMap<String, Value>>,
}
pub fn factory(c: &Connection, token: Zeroizing<String>) -> Result<Arc<dyn GitProvider>> {
    let c = c.checked()?;
    let http = Arc::new(Https::new(&c, token)?);
    Ok(match c.provider()? {
        Provider::Gitea => Arc::new(Gitea {
            http,
            trees: Mutex::new(BTreeMap::new()),
        }),
        Provider::Github => {
            let (owner, repo) = c.repository.split_once('/').unwrap();
            Arc::new(Github {
                http,
                owner: owner.into(),
                repo: repo.into(),
                trees: Mutex::new(BTreeMap::new()),
                commits: Mutex::new(BTreeMap::new()),
            })
        }
    })
}
fn entries(v: &Value, github: bool) -> Result<Vec<Entry>> {
    let mut names = BTreeSet::new();
    let mut result = vec![];
    for row in v.as_array().ok_or_else(malformed)? {
        let name = string(row, "path")?;
        relative_path(name)?;
        if name.contains('/') || !names.insert(name.to_owned()) {
            return Err(malformed());
        }
        let kind = match (string(row, "type")?, string(row, "mode")?) {
            ("tree", "040000") => EntryKind::Tree,
            ("blob", "100644" | "100755") => EntryKind::Blob,
            _ => EntryKind::Unsupported,
        };
        let size = if kind == EntryKind::Blob {
            row.get("size")
                .and_then(Value::as_u64)
                .ok_or_else(malformed)?
        } else {
            0
        };
        result.push(Entry {
            name: name.into(),
            oid: oid(string(row, "sha")?, github)?,
            size,
            kind,
            mode: string(row, "mode")?.into(),
        });
    }
    Ok(result)
}
fn cache_put<T: Clone>(
    cache: &Mutex<BTreeMap<String, T>>,
    id: &str,
    value: T,
    max: usize,
) -> Result<T> {
    let mut c = cache.lock().map_err(crate::external)?;
    if c.len() >= max {
        c.pop_first();
    }
    c.insert(id.into(), value.clone());
    Ok(value)
}
fn branch_list(http: &dyn Http, github: bool, cancel: &Cancellation) -> Result<Vec<String>> {
    let mut result = vec![];
    let mut seen = BTreeSet::<String>::new();
    for page in 1..=10000 {
        let value = http.json(
            "branches",
            &[
                ("page".into(), page.to_string()),
                (
                    if github { "per_page" } else { "limit" }.into(),
                    if github { "100" } else { "50" }.into(),
                ),
            ],
            None,
            cancel,
        )?;
        let rows = value.as_array().ok_or_else(malformed)?;
        if rows.is_empty() {
            return Ok(result);
        }
        let old = seen.len();
        for row in rows {
            let name = string(row, "name")?;
            validate_branch(name)?;
            if seen.insert(name.into()) {
                result.push(name.into());
            } else if github {
                return Err(malformed());
            }
        }
        if old == seen.len() {
            return Err(malformed());
        }
    }
    Err(invalid("Достигнут лимит страниц веток"))
}
fn history(
    http: &dyn Http,
    github: bool,
    revision: &str,
    since: Option<&str>,
    until: &str,
    cancel: &Cancellation,
    progress: &dyn Fn(usize),
) -> Result<Vec<Commit>> {
    let mut out = vec![];
    let mut seen = BTreeSet::new();
    for page in 1..=10000 {
        cancel.check()?;
        let mut q = vec![
            ("sha".into(), revision.into()),
            ("page".into(), page.to_string()),
            (
                if github { "per_page" } else { "limit" }.into(),
                if github { "100" } else { "50" }.into(),
            ),
            ("until".into(), until.into()),
        ];
        if !github {
            for name in ["files", "stat", "verification"] {
                q.push((name.into(), "false".into()));
            }
        }
        if let Some(s) = since {
            q.push(("since".into(), s.into()));
        }
        let value = http.json("commits", &q, None, cancel)?;
        let rows = value.as_array().ok_or_else(malformed)?;
        if rows.is_empty() {
            return Ok(out);
        }
        for row in rows {
            let id = oid(string(row, "sha")?, github)?;
            if !seen.insert(id.clone()) {
                return Err(malformed());
            }
            out.push(Commit {
                oid: id,
                message: string(&row["commit"], "message")?.into(),
                date: row["commit"]["committer"]["date"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
            });
        }
        progress(out.len());
    }
    Err(invalid("Достигнут лимит страниц коммитов"))
}
impl GitProvider for Gitea {
    fn branches(&self, c: &Cancellation) -> Result<Vec<String>> {
        branch_list(self.http.as_ref(), false, c)
    }
    fn head(&self, branch: &str, c: &Cancellation) -> Result<String> {
        validate_branch(branch)?;
        let value = self
            .http
            .json(&format!("git/refs/heads/{}", quote(branch)), &[], None, c)?;
        let name = format!("refs/heads/{branch}");
        let row = if let Some(rows) = value.as_array() {
            rows.iter()
                .find(|r| r["ref"] == name)
                .ok_or_else(malformed)?
        } else {
            &value
        };
        if row["ref"] != name {
            return Err(malformed());
        }
        oid(string(&row["object"], "sha")?, false)
    }
    fn root(&self, revision: &str, _: &Cancellation) -> Result<String> {
        oid(revision, false)
    }
    fn tree(&self, id: &str, c: &Cancellation) -> Result<Vec<Entry>> {
        if let Some(v) = self.trees.lock().map_err(crate::external)?.get(id) {
            return Ok(v.clone());
        }
        let mut out = vec![];
        let mut names = BTreeSet::new();
        let mut total = None;
        for page in 1..=10000 {
            let value = self.http.json(
                &format!("git/trees/{}", oid(id, false)?),
                &[
                    ("page".into(), page.to_string()),
                    ("per_page".into(), "1000".into()),
                ],
                None,
                c,
            )?;
            if string(&value, "sha")? != id || value["page"].as_u64() != Some(page) {
                return Err(malformed());
            }
            let count = value["total_count"].as_u64().ok_or_else(malformed)?;
            if total.is_some_and(|n| n != count) {
                return Err(malformed());
            }
            total = Some(count);
            let rows = entries(&value["tree"], false)?;
            let truncated = value["truncated"].as_bool().ok_or_else(malformed)?;
            if truncated && rows.is_empty() {
                return Err(malformed());
            }
            for row in rows {
                if !names.insert(row.name.clone()) {
                    return Err(malformed());
                }
                out.push(row);
            }
            if !truncated {
                if out.len() as u64 != count {
                    return Err(malformed());
                }
                return cache_put(&self.trees, id, out, 128);
            }
        }
        Err(malformed())
    }
    fn recursive_tree(
        &self,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<RecursiveListing<Vec<RecursiveRow>>> {
        crate::git_recursive::gitea_listing(self.http.as_ref(), id, cancel)
    }
    fn file(&self, revision: &str, path: &str, e: &Entry, c: &Cancellation) -> Result<Vec<u8>> {
        relative_path(path)?;
        let bytes = self.http.bytes(
            &format!("raw/{}", quote(path)),
            &[("ref".into(), oid(revision, false)?)],
            None,
            file_read_limit(e.size)?,
            true,
            c,
        )?;
        verify_blob(&bytes, &e.oid, e.size)?;
        Ok(bytes)
    }
    fn files(
        &self,
        revision: &str,
        files: &[(String, Entry)],
        c: &Cancellation,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut output = BTreeMap::new();
        let mut start = 0;
        while start < files.len() {
            let mut end = start;
            while end < files.len() && end - start < 20 {
                let paths = files[start..=end]
                    .iter()
                    .map(|(p, _)| p)
                    .collect::<Vec<_>>();
                let body = json!({"files":paths}).to_string();
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("ref", revision)
                    .append_pair("body", &body)
                    .finish();
                if end > start && query.len() > 4500 {
                    break;
                }
                end += 1;
            }
            let batch = &files[start..end];
            let paths = batch.iter().map(|(p, _)| p).collect::<Vec<_>>();
            let value = self.http.json(
                "file-contents",
                &[
                    ("ref".into(), revision.into()),
                    ("body".into(), json!({"files":paths}).to_string()),
                ],
                None,
                c,
            )?;
            for row in value.as_array().ok_or_else(malformed)? {
                let Some(path) = row["path"].as_str() else {
                    continue;
                };
                let Some((_, entry)) = batch.iter().find(|(p, _)| p == path) else {
                    continue;
                };
                if row["encoding"] != "base64" {
                    continue;
                }
                if output.contains_key(path) {
                    return Err(malformed());
                }
                let data = STANDARD
                    .decode(string(row, "content")?)
                    .map_err(|_| malformed())?;
                verify_blob(&data, &entry.oid, entry.size)?;
                output.insert(path.into(), data);
            }
            for (path, e) in batch {
                if !output.contains_key(path) {
                    output.insert(path.clone(), self.file(revision, path, e, c)?);
                }
            }
            start = end;
        }
        Ok(output)
    }
    fn commits(
        &self,
        r: &str,
        s: Option<&str>,
        u: &str,
        c: &Cancellation,
        p: &dyn Fn(usize),
    ) -> Result<Vec<Commit>> {
        history(self.http.as_ref(), false, r, s, u, c, p)
    }
    fn changed(&self, id: &str, _: &str, c: &Cancellation) -> Result<Vec<String>> {
        let row = self.http.json(
            &format!("git/commits/{}", oid(id, false)?),
            &[
                ("files".into(), "true".into()),
                ("stat".into(), "false".into()),
                ("verification".into(), "false".into()),
            ],
            None,
            c,
        )?;
        if string(&row, "sha")? != id {
            return Err(malformed());
        }
        row["files"]
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .map(|v| Ok(relative_path(string(v, "filename")?)?.into()))
            .collect()
    }
}
impl Github {
    fn commit(&self, id: &str, c: &Cancellation) -> Result<Value> {
        if let Some(v) = self.commits.lock().map_err(crate::external)?.get(id) {
            return Ok(v.clone());
        }
        let row = self
            .http
            .json(&format!("git/commits/{}", oid(id, true)?), &[], None, c)?;
        if string(&row, "sha")? != id {
            return Err(malformed());
        }
        oid(string(&row["tree"], "sha")?, true)?;
        for p in row["parents"].as_array().ok_or_else(malformed)? {
            oid(string(p, "sha")?, true)?;
        }
        cache_put(&self.commits, id, row, 256)
    }
    fn subtree(&self, root: Option<String>, dir: &str, c: &Cancellation) -> Result<Option<String>> {
        let mut current = root;
        for name in dir.split('/') {
            let Some(id) = current else { return Ok(None) };
            current = self
                .tree(&id, c)?
                .into_iter()
                .find(|e| e.name == name)
                .map(|e| {
                    if e.kind == EntryKind::Tree {
                        Ok(e.oid)
                    } else {
                        Err(malformed())
                    }
                })
                .transpose()?;
        }
        Ok(current)
    }
}
impl GitProvider for Github {
    fn branches(&self, c: &Cancellation) -> Result<Vec<String>> {
        branch_list(self.http.as_ref(), true, c)
    }
    fn head(&self, branch: &str, c: &Cancellation) -> Result<String> {
        validate_branch(branch)?;
        let v = self
            .http
            .json(&format!("git/ref/heads/{}", quote(branch)), &[], None, c)?;
        if v["ref"] != format!("refs/heads/{branch}") || v["object"]["type"] != "commit" {
            return Err(malformed());
        }
        oid(string(&v["object"], "sha")?, true)
    }
    fn root(&self, r: &str, c: &Cancellation) -> Result<String> {
        Ok(string(&self.commit(r, c)?["tree"], "sha")?.into())
    }
    fn tree(&self, id: &str, c: &Cancellation) -> Result<Vec<Entry>> {
        if let Some(v) = self.trees.lock().map_err(crate::external)?.get(id) {
            return Ok(v.clone());
        }
        let v = self
            .http
            .json(&format!("git/trees/{}", oid(id, true)?), &[], None, c)?;
        if string(&v, "sha")? != id || v["truncated"] != false {
            return Err(malformed());
        }
        cache_put(&self.trees, id, entries(&v["tree"], true)?, 512)
    }
    fn file(&self, _: &str, _: &str, e: &Entry, c: &Cancellation) -> Result<Vec<u8>> {
        if e.size > 100 * 1024 * 1024 {
            return Err(invalid("Blob GitHub превышает 100 МиБ"));
        }
        let data = self.http.bytes(
            &format!("git/blobs/{}", oid(&e.oid, true)?),
            &[],
            None,
            file_read_limit(e.size)?,
            true,
            c,
        )?;
        verify_blob(&data, &e.oid, e.size)?;
        Ok(data)
    }
    fn files(
        &self,
        revision: &str,
        files: &[(String, Entry)],
        c: &Cancellation,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut result = BTreeMap::new();
        let mut pending = vec![];
        let flush = |batch: &[(String, Entry)],
                     result: &mut BTreeMap<String, Vec<u8>>|
         -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let vars = batch
                .iter()
                .enumerate()
                .map(|(i, _)| format!("$oid{i}: GitObjectID!"))
                .collect::<Vec<_>>()
                .join(", ");
            let fields=batch.iter().enumerate().map(|(i,_)|format!("b{i}: object(oid: $oid{i}) {{ __typename ... on Blob {{ oid byteSize isBinary isTruncated text }} }}")).collect::<Vec<_>>().join(" ");
            let query = format!(
                "query($owner: String!, $repo: String!, {vars}) {{ repository(owner:$owner, name:$repo) {{ {fields} }} rateLimit {{ cost remaining resetAt }} }}"
            );
            let mut variables = json!({"owner":self.owner,"repo":self.repo});
            for (i, (_, e)) in batch.iter().enumerate() {
                variables[format!("oid{i}")] = json!(e.oid);
            }
            let v = self.http.json(
                "/graphql",
                &[],
                Some(json!({"query":query,"variables":variables})),
                c,
            )?;
            if v["errors"].as_array().is_some_and(|a| !a.is_empty()) {
                return Err(invalid(
                    "GitHub GraphQL вернул ошибку; частичный ответ не используется",
                ));
            }
            if !v["data"]["repository"].is_object() {
                return Err(malformed());
            }
            for (i, (path, e)) in batch.iter().enumerate() {
                let blob = &v["data"]["repository"][format!("b{i}")];
                if blob["__typename"] != "Blob"
                    || blob["oid"] != e.oid
                    || blob["byteSize"].as_u64() != Some(e.size)
                {
                    return Err(malformed());
                }
                let data = blob["text"].as_str().map(|s| s.as_bytes().to_vec());
                let data = match data {
                    Some(data)
                        if blob["isBinary"] == false
                            && blob["isTruncated"] == false
                            && verify_blob(&data, &e.oid, e.size).is_ok() =>
                    {
                        data
                    }
                    _ => self.file(revision, path, e, c)?,
                };
                if result.insert(path.clone(), data).is_some() {
                    return Err(malformed());
                }
            }
            Ok(())
        };
        let mut size = 0;
        for (path, e) in files {
            if e.size > 2 * 1024 * 1024 {
                flush(&pending, &mut result)?;
                pending.clear();
                size = 0;
                result.insert(path.clone(), self.file(revision, path, e, c)?);
                continue;
            }
            if pending.len() >= 20 || size + e.size > 2 * 1024 * 1024 {
                flush(&pending, &mut result)?;
                pending.clear();
                size = 0;
            }
            pending.push((path.clone(), e.clone()));
            size += e.size;
        }
        flush(&pending, &mut result)?;
        Ok(result)
    }
    fn commits(
        &self,
        r: &str,
        s: Option<&str>,
        u: &str,
        c: &Cancellation,
        p: &dyn Fn(usize),
    ) -> Result<Vec<Commit>> {
        history(self.http.as_ref(), true, r, s, u, c, p)
    }
    fn changed(&self, id: &str, dir: &str, c: &Cancellation) -> Result<Vec<String>> {
        let current = self.commit(id, c)?;
        let parent = current["parents"]
            .as_array()
            .unwrap()
            .first()
            .map(|v| string(v, "sha"))
            .transpose()?;
        let before = parent.map(|p| self.root(p, c)).transpose()?;
        let a = self.subtree(before, dir, c)?;
        let b = self.subtree(Some(string(&current["tree"], "sha")?.into()), dir, c)?;
        let mut todo = vec![(dir.to_owned(), a, b)];
        let mut output = BTreeSet::new();
        while let Some((base, a, b)) = todo.pop() {
            c.check()?;
            if a == b {
                continue;
            }
            let left = match a {
                Some(s) => self.tree(&s, c)?,
                None => vec![],
            }
            .into_iter()
            .map(|e| (e.name.clone(), e))
            .collect::<BTreeMap<_, _>>();
            let right = match b {
                Some(s) => self.tree(&s, c)?,
                None => vec![],
            }
            .into_iter()
            .map(|e| (e.name.clone(), e))
            .collect::<BTreeMap<_, _>>();
            for name in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
                let a = left.get(name);
                let b = right.get(name);
                if a.zip(b)
                    .is_some_and(|(a, b)| a.oid == b.oid && a.kind == b.kind && a.mode == b.mode)
                {
                    continue;
                }
                if a.into_iter()
                    .chain(b)
                    .any(|e| e.kind == EntryKind::Unsupported)
                {
                    return Err(invalid("Неподдерживаемый тип файла в изменениях Git"));
                }
                let path = format!("{base}/{name}");
                if a.into_iter().chain(b).any(|e| e.kind == EntryKind::Blob) {
                    output.insert(path.clone());
                }
                let tree = |e: Option<&Entry>| {
                    e.filter(|e| e.kind == EntryKind::Tree)
                        .map(|e| e.oid.clone())
                };
                if tree(a).is_some() || tree(b).is_some() {
                    todo.push((path, tree(a), tree(b)));
                }
            }
        }
        Ok(output.into_iter().collect())
    }
}
