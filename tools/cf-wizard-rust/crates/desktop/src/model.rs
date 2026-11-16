//! Presentation state. IO executes in jobs; dependency and build decisions live in services.
use cf_application::{BuildOptions, Cancellation, DependencyEngine, MetadataSource, PreparedPlan};
use cf_domain::{BuildMode, Error, Result};
use cf_infrastructure::{
    engine::RustEngine,
    git::{self, Connection, GitProvider},
    git_source::{GitSource, TaskResult},
    index::{IndexReader, Proof},
    indexing::{self, IndexStatus},
    journal::{self, OperationLog},
    output::Output,
    platform::{self, Platform},
    settings::{self, Settings},
    source::{LocalSource, normal_path},
    vault,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tao::event_loop::EventLoopProxy;
use zeroize::Zeroizing;
pub type Update = Box<dyn FnOnce(&mut Model) + Send>;
type StubGroups = BTreeMap<String, BTreeMap<String, Vec<String>>>;

fn stub_groups(stubs: &BTreeMap<String, cf_domain::Stub>) -> StubGroups {
    let mut groups = StubGroups::new();
    for (key, stub) in stubs {
        let kind = key.split('.').next().unwrap_or("");
        let owners = stub
            .causes
            .iter()
            .map(|cause| cause.owner.clone())
            .collect::<BTreeSet<_>>();
        groups
            .entry(kind.into())
            .or_default()
            .insert(key.clone(), owners.into_iter().collect());
    }
    groups
}

fn preserve_operation_error<T>(operation: Result<T>, log_write: Result<()>) -> Result<T> {
    operation.and_then(|value| log_write.map(|()| value))
}

pub enum Event {
    Rpc(Value),
    Done {
        generation: u64,
        callback: Option<u64>,
        outcome: Result<Update>,
        log: Option<Arc<OperationLog>>,
    },
    Progress(String, Value),
    Poll(u64, Result<String>),
    Platforms(Vec<Platform>),
    PlatformChecked(String, String, Result<Platform>),
}
pub struct Model {
    pub preferences: Settings,
    pub settings_path: PathBuf,
    pub values: BTreeMap<String, String>,
    pub source: Option<Arc<dyn MetadataSource>>,
    pub git: Option<Arc<GitSource>>,
    pub client: Option<Arc<dyn GitProvider>>,
    pub branches: Vec<String>,
    pub platforms: Vec<Platform>,
    pub selected: BTreeSet<String>,
    pub excluded: BTreeSet<String>,
    pub plan: Option<Box<dyn PreparedPlan>>,
    pub index: Option<IndexStatus>,
    pub package: Option<PathBuf>,
    pub last_log: Option<PathBuf>,
    pub task_filter: Option<TaskResult>,
    pub busy: bool,
    pub status: String,
    pub status_kind: String,
    pub generation: u64,
    pub catalog_revision: u64,
    pub task_revision: u64,
    pub cancel: Cancellation,
    pub task_scope: String,
    pub cancelling: bool,
    pub poll_info: String,
    pub next_poll: Instant,
    pub polling: bool,
    pub proxy: EventLoopProxy<Event>,
    pub notices: Vec<(String, String)>,
    pub signals: Vec<(String, Vec<Value>)>,
    pub dirty: Option<Instant>,
    pub platform_check: bool,
    pub opened_identity: Option<String>,
    pub labels: Vec<(String, (String, String))>,
}
fn field(v: &Value, n: usize) -> &str {
    v[n].as_str().unwrap_or("")
}
pub fn mode(value: &str) -> BuildMode {
    match value {
        "ibcmd" => BuildMode::Ibcmd,
        "designer" => BuildMode::Designer,
        _ => BuildMode::Auto,
    }
}
impl Model {
    pub fn new(path: PathBuf, proxy: EventLoopProxy<Event>) -> Self {
        let app_base = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(settings::base);
        let preferences = Settings::load(&path);
        let mut values = preferences.values.clone();
        for key in [
            "export",
            "destination",
            "platform",
            "designer",
            "logs",
            "index_directory",
            "task",
        ] {
            values.entry(key.into()).or_default();
        }
        if values["logs"].is_empty() {
            values.insert(
                "logs".into(),
                app_base.join("logs").to_string_lossy().into(),
            );
        }
        if values["index_directory"].is_empty() {
            values.insert(
                "index_directory".into(),
                app_base.join("indexes").to_string_lossy().into(),
            );
        }
        let mut preferences = preferences;
        if preferences.git_settings.cache.is_empty() {
            preferences.git_settings.cache = app_base.join("git-cache").to_string_lossy().into();
        }
        Self {
            preferences,
            settings_path: path,
            values,
            source: None,
            git: None,
            client: None,
            branches: vec![],
            platforms: vec![],
            selected: BTreeSet::new(),
            excluded: BTreeSet::new(),
            plan: None,
            index: None,
            package: None,
            last_log: None,
            task_filter: None,
            busy: false,
            status: "Откройте XML-выгрузку или подключитесь к Git.".into(),
            status_kind: "idle".into(),
            generation: 0,
            catalog_revision: 0,
            task_revision: 0,
            cancel: Cancellation::default(),
            task_scope: String::new(),
            cancelling: false,
            poll_info: String::new(),
            next_poll: Instant::now() + Duration::from_secs(60),
            polling: false,
            proxy,
            notices: vec![],
            signals: vec![],
            dirty: None,
            platform_check: false,
            opened_identity: None,
            labels: serde_json::from_str(include_str!("../ui/labels.json"))
                .expect("embedded labels"),
        }
    }
    pub fn object(&self, key: &str) -> Value {
        let (kind, name) = key.split_once('.').unwrap_or(("", key));
        let label = self
            .labels
            .iter()
            .find(|(k, _)| k == kind)
            .map_or(kind, |(_, (_, s))| s.as_str());
        json!({"key":key,"name":name,"type":label})
    }
    fn group_label(&self, kind: &str) -> String {
        self.labels
            .iter()
            .find(|(k, _)| k == kind)
            .map_or_else(|| kind.into(), |(_, (s, _))| s.clone())
    }
    pub fn platform(&self) -> Option<Platform> {
        let mode = mode(&self.preferences.build_mode);
        if mode == BuildMode::Auto {
            self.platforms
                .iter()
                .find(|p| p.backend == "designer" && p.version == "8.3.27.2214")
                .cloned()
        } else {
            let key = if mode == BuildMode::Ibcmd {
                "platform"
            } else {
                "designer"
            };
            let path = Path::new(&self.values[key]);
            self.platforms
                .iter()
                .find(|p| normal_path(&p.path) == normal_path(path))
                .cloned()
        }
    }
    pub fn snapshot(&self) -> Value {
        let p = self.platform();
        let source = self.source.as_ref();
        let git = self.git.as_ref();
        let ready = self.index.as_ref().is_some_and(|i| i.ready);
        let index_found = self.index.as_ref().is_some_and(|i| i.found);
        let mut values = self.values.clone();
        values.insert("build_mode".into(), self.preferences.build_mode.clone());
        values.insert(
            "platform_path".into(),
            p.as_ref()
                .map(|p| p.path.to_string_lossy().into())
                .unwrap_or_else(|| {
                    if self.preferences.build_mode == "auto" {
                        String::new()
                    } else {
                        self.values[if self.preferences.build_mode == "ibcmd" {
                            "platform"
                        } else {
                            "designer"
                        }]
                        .clone()
                    }
                }),
        );
        let git_ready = self.preferences.source_mode == "git"
            && git.is_some_and(|g| g.connection().branch == self.preferences.git_settings.branch);
        json!({"values":values,"platforms":self.platforms.iter().map(|p|p.path.to_string_lossy()).collect::<Vec<_>>(),"platformPlaceholder":"Выберите установленную платформу 1С x64","platformLabel":p.map(|p|format!("{} · {}",if p.backend=="ibcmd"{"ibcmd"}else{"Конфигуратор"},p.version)).unwrap_or_default(),
            "configuration":source.map(|s|s.catalog().name.as_str()).unwrap_or(""),
            "activeGit":git.is_some(),"activeSource":git.map(|g|format!("{} · {} · {}",if g.connection().provider=="github"{"GitHub"}else{"Gitea"},g.connection().branch,&g.revision()[..12])).unwrap_or_else(||"Локальная папка".into()),
            "catalogRevision":self.catalog_revision,"sourceMode":self.preferences.source_mode,"gitBranches":self.branches,"gitBranch":self.preferences.git_settings.branch,"gitLocation":format!("{} / {}",self.preferences.git_settings.repository,self.preferences.git_settings.directory),"gitPollInfo":self.poll_info,
            "gitTaskReady":git_ready,"gitTaskRevision":self.task_revision,"gitTaskScope":self.task_scope,"gitTaskCancelling":self.cancelling,
            "gitTaskResult":self.task_filter.as_ref().map(|r|json!({"scope":format!("{} · {}",self.preferences.git_settings.branch,&r.revision[..12]),"commits":r.commits.len(),"files":r.files,"unmapped":r.missing.len(),"objects":r.objects.len(),"canMark":r.objects.iter().any(|k|!self.selected.contains(k))})),
            "gitOpenText":if git_ready{"Обновить ветку"}else{"Открыть ветку"},
            "selected":self.selected.iter().map(|k|self.object(k)).collect::<Vec<_>>(),"excluded":self.excluded,"stubTotal":self.plan.as_ref().map_or(0,|p|p.composition().stubs.len()),"busy":self.busy,
            "status":self.status,"statusKind":self.status_kind,"canPreview":!self.selected.is_empty()&&!self.busy&&(git.is_none()||git_ready&&ready),
            "canBuild":self.plan.as_ref().is_some_and(|p|p.composition().complete())&&self.platform().is_some()&&!self.values["destination"].trim().is_empty()&&!self.busy,
            "dependencyErrors":self.plan.as_ref().map(|p|&p.composition().diagnostics),
            "canMarkStubs":self.plan.is_some()&&!self.busy,"canOpenPackage":self.package.is_some()&&!self.busy,"canOpenJournal":self.last_log.is_some()&&!self.busy,
            "canRefreshIndex":source.is_some()&&!self.busy,"indexExists":index_found,"indexReady":ready,"indexInfo":self.index.as_ref().map(|i|i.info.as_str()).unwrap_or("Индекс ещё не проверен.")})
    }
    pub fn stubs(&self) -> Value {
        let Some(plan) = &self.plan else {
            return json!([]);
        };
        let mut groups = stub_groups(&plan.composition().stubs);
        let mut out = vec![];
        for (kind, _) in &self.labels {
            if let Some(rows) = groups.remove(kind) {
                let rows = rows
                    .into_iter()
                    .map(|(key, owners)| {
                        let mut v = self.object(&key);
                        v["required"] = json!(plan.composition().stubs[&key].required());
                        v["reason"] = json!(
                            owners
                                .iter()
                                .map(|o| o.split_once('.').map_or(o.as_str(), |(_, n)| n))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        v["owners"] = json!(owners.join("\n"));
                        v
                    })
                    .collect::<Vec<_>>();
                out.push(json!({"kind":kind,"label":self.group_label(kind),"rows":rows}));
            }
        }
        json!(out)
    }
    fn filtered(&self, query: &str, selected_only: bool) -> BTreeMap<String, Vec<String>> {
        let Some(src) = &self.source else {
            return BTreeMap::new();
        };
        let query = query.trim().to_lowercase();
        let mut groups = BTreeMap::<String, Vec<String>>::new();
        let task = self.task_filter.as_ref().map(|r| {
            r.objects
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        });
        for key in src.catalog().objects.keys() {
            if key.kind() == "Language"
                || selected_only && !self.selected.contains(&key.0)
                || task.as_ref().is_some_and(|r| !r.contains(key.0.as_str()))
            {
                continue;
            }
            let obj = self.object(&key.0);
            if !query.is_empty()
                && !format!(
                    "{} {} {}",
                    key.0,
                    obj["type"].as_str().unwrap_or(""),
                    self.group_label(key.kind())
                )
                .to_lowercase()
                .contains(&query)
            {
                continue;
            }
            groups
                .entry(key.kind().into())
                .or_default()
                .push(key.0.clone());
        }
        groups
    }
    fn save_soon(&mut self) {
        self.dirty = Some(Instant::now());
    }
    pub fn flush(&mut self) {
        if self.dirty.take().is_none() {
            return;
        }
        for key in [
            "export",
            "destination",
            "platform",
            "designer",
            "logs",
            "index_directory",
        ] {
            self.preferences
                .values
                .insert(key.into(), self.values[key].clone());
        }
        if let Err(e) = self.preferences.save(&self.settings_path) {
            self.notice("Настройки", e.to_string());
        }
        if std::mem::take(&mut self.platform_check) {
            let key = if self.preferences.build_mode == "ibcmd" {
                "platform"
            } else {
                "designer"
            }
            .to_owned();
            let path = self.values[&key].clone();
            let proxy = self.proxy.clone();
            if !path.trim().is_empty() {
                std::thread::spawn(move || {
                    let result = platform::validate(
                        Path::new(&path),
                        if key == "platform" {
                            BuildMode::Ibcmd
                        } else {
                            BuildMode::Designer
                        },
                    );
                    let _ = proxy.send_event(Event::PlatformChecked(key, path, result));
                });
            }
        }
    }
    pub fn add_platforms(&mut self, rows: Vec<Platform>) {
        for p in rows {
            self.platforms
                .retain(|old| normal_path(&old.path) != normal_path(&p.path));
            self.platforms.push(p);
        }
    }
    pub fn notice(&mut self, title: impl Into<String>, text: impl Into<String>) {
        self.notices.push((title.into(), text.into()));
    }
    fn invalidate_plan(&mut self) {
        self.plan = None;
        self.package = None;
        self.excluded.clear();
        self.signals.push(("stubsChanged".into(), vec![json!([])]));
    }
    fn invalidate_source(&mut self) {
        self.invalidate_plan();
        self.source = None;
        self.git = None;
        self.index = None;
        self.selected.clear();
        self.task_filter = None;
        self.catalog_revision += 1;
        self.task_revision += 1;
        self.generation += 1;
    }
    fn start(
        &mut self,
        caption: &str,
        callback: Option<u64>,
        operation: Option<&str>,
        job: impl FnOnce(Cancellation, Option<Arc<OperationLog>>) -> Result<Update> + Send + 'static,
    ) -> Result<()> {
        if self.busy {
            return Err(Error::Invalid(
                "Дождитесь завершения текущей операции".into(),
            ));
        }
        let log = operation
            .map(|name| OperationLog::create(Path::new(&self.values["logs"]), name).map(Arc::new))
            .transpose()?;
        if let Some(log) = &log {
            self.last_log = Some(log.path.clone());
        }
        self.busy = true;
        self.status = caption.into();
        self.status_kind = "busy".into();
        self.cancel = Cancellation::default();
        self.cancelling = false;
        let cancel = self.cancel.clone();
        let generation = self.generation;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(cancel, log.clone())))
                    .unwrap_or_else(|_| {
                        Err(Error::External(
                            "Аварийная ошибка фоновой операции. Подробности в папке «Ошибки»."
                                .into(),
                        ))
                    });
            if let (Err(e), Some(log)) = (&result, &log) {
                let _ = log.failure(&e.to_string());
            }
            let _ = proxy.send_event(Event::Done {
                generation,
                callback,
                outcome: result,
                log,
            });
        });
        Ok(())
    }
    fn install_source(
        &mut self,
        source: Arc<dyn MetadataSource>,
        git: Option<Arc<GitSource>>,
        status: IndexStatus,
        directory: String,
    ) {
        let old = self.opened_identity.clone();
        let key = source
            .manifest()
            .map_or(source.identity(), |m| m.source_key.as_str())
            .to_owned();
        self.opened_identity = Some(key.clone());
        self.generation += 1;
        self.invalidate_plan();
        self.selected.clear();
        self.task_filter = None;
        self.task_revision += 1;
        self.catalog_revision += 1;
        self.values
            .insert("index_directory".into(), directory.clone());
        self.source = Some(source);
        self.git = git;
        self.index = Some(status);
        self.poll_info.clear();
        self.next_poll = Instant::now()
            + Duration::from_secs(self.preferences.git_settings.interval.max(30) as u64);
        self.preferences
            .index_directories
            .insert(key.clone(), directory.clone());
        self.save_soon();
        self.status = "Выгрузка открыта. Отметьте объекты для переноса.".into();
        self.status_kind = "ok".into();
        if old.is_some_and(|o| o != key) {
            self.signals
                .push(("indexFolderPrompt".into(), vec![json!(directory)]));
        }
    }
    pub fn open(&mut self, callback: Option<u64>) -> Result<()> {
        if self.preferences.source_mode == "local" {
            if self.values["export"].trim().is_empty() {
                return self.pick("cf-source".into(), callback);
            }
            let path = PathBuf::from(self.values["export"].trim());
            let directories = self.preferences.index_directories.clone();
            let fallback = self.values["index_directory"].clone();
            self.start("Открываю XML-выгрузку…",callback,Some("local-open"),move|c,log|{
                c.check()?;let source:Arc<dyn MetadataSource>=Arc::new(LocalSource::open(&path)?);
                let folder=directories.get(source.identity()).cloned().unwrap_or(fallback);
                let index=indexing::status(source.as_ref(),Path::new(&folder))?;
                if let Some(log)=log{log.value("source-opened",json!({"source":source.identity(),"configuration":source.catalog().name,"indexReady":index.ready}))?;}
                let mut platforms=platform::discover(BuildMode::Designer);platforms.extend(platform::discover(BuildMode::Ibcmd));
                Ok(Box::new(move|m|{m.add_platforms(platforms);m.install_source(source,None,index,folder);}))
            })
        } else {
            let connection = self.preferences.git_settings.clone();
            let client = if let Some(client) = &self.client {
                client.clone()
            } else {
                let target = vault::target(connection.provider()?, &connection.server)?;
                let token = vault::load(&target)?.ok_or_else(|| {
                    Error::Invalid("Откройте «Настройки → Подключение Git» и введите токен.".into())
                })?;
                git::factory(&connection, token)?
            };
            let directories = self.preferences.index_directories.clone();
            let fallback = self.values["index_directory"].clone();
            self.start("Читаю закреплённую ревизию Git…",callback,Some("git-open"),move|c,log|{
                let branches=client.branches(&c)?;let source=Arc::new(GitSource::open(&connection,client.clone(),&c)?);
                let folder=directories.get(source.preference_key()).cloned().unwrap_or(fallback);
                let index=indexing::status(source.as_ref(),Path::new(&folder))?;
                if let Some(l)=log{l.value("source-opened",json!({"source":source.preference_key(),"revision":source.revision(),"indexReady":index.ready}))?;}
                let mut platforms=platform::discover(BuildMode::Designer);platforms.extend(platform::discover(BuildMode::Ibcmd));
                Ok(Box::new(move|m|{m.add_platforms(platforms);m.client=Some(client);m.branches=branches;m.install_source(source.clone(),Some(source),index,folder);}))
            })
        }
    }
    fn refresh(&mut self, callback: Option<u64>) -> Result<()> {
        let source = self
            .source
            .clone()
            .ok_or_else(|| Error::Invalid("Откройте выгрузку".into()))?;
        let directory = self.values["index_directory"].clone();
        let proxy = self.proxy.clone();
        self.invalidate_plan();
        self.start(
            "Обновляю индекс зависимостей…",
            callback,
            Some("index"),
            move |c, log| {
                let report = indexing::update(
                    source.as_ref(),
                    Path::new(&directory),
                    &c,
                    &|done, total| {
                        if done % 100 == 0 || done == total {
                            let _ = proxy.send_event(Event::Progress(
                                "index".into(),
                                json!({"done":done,"total":total}),
                            ));
                        }
                    },
                )?;
                let status = indexing::status(source.as_ref(), Path::new(&directory))?;
                if let Some(log) = log {
                    log.value("index-updated", json!(report))?;
                }
                Ok(Box::new(move |m| {
                    m.index = Some(status);
                    m.status = format!(
                        "Индекс готов · добавлено {} · изменено {} · удалено {} · XML прочитано {}",
                        report.added, report.changed, report.removed, report.xml_reads
                    );
                    m.status_kind = "ok".into();
                }))
            },
        )
    }
    fn preview(&mut self, callback: Option<u64>) -> Result<()> {
        let source = self
            .source
            .clone()
            .ok_or_else(|| Error::Invalid("Откройте выгрузку".into()))?;
        if self.git.is_some() && !self.index.as_ref().is_some_and(|i| i.ready) {
            return Err(Error::Invalid(
                "Сначала создайте или обновите индекс Git".into(),
            ));
        }
        let index = self
            .index
            .as_ref()
            .filter(|i| i.ready)
            .and_then(|i| i.path.clone());
        let selected = self.selected.clone();
        let excluded = self.excluded.clone();
        self.start("Определяю зависимости…",callback,Some("preview"),move|c,log|{
            let index_started=Instant::now();
            let opened=index.map(|path|IndexReader::open(&path,source.manifest().map_or(Proof::Local(source.identity()),Proof::Git))).transpose();
            let index_open_ms=index_started.elapsed().as_secs_f64()*1000.;
            let index_log=log.as_ref().map_or(Ok(()),|log|log.value("preview-index-open",json!({"milliseconds":index_open_ms,"success":opened.is_ok(),"hasIndex":matches!(&opened,Ok(Some(_)))})));
            let reader=preserve_operation_error(opened,index_log)?;
            let engine_started=Instant::now();
            let prepared=RustEngine.preview(source.as_ref(),reader.as_ref().map(|r|r as &dyn cf_application::FactStore),&selected,&excluded,&c);
            let engine_ms=engine_started.elapsed().as_secs_f64()*1000.;
            let engine_log=log.as_ref().map_or(Ok(()),|log|log.value("preview-engine",json!({"milliseconds":engine_ms,"success":prepared.is_ok()})));
            let plan=preserve_operation_error(prepared,engine_log)?;
            if let Some(log)=log{log.value("dependencies-ready",json!({"kernel":"datafrog-2.0.1","selected":plan.composition().selected,"stubs":plan.composition().stubs,"warnings":plan.composition().warnings,"diagnostics":plan.composition().diagnostics,"choices":plan.composition().choices,"immediateReasons":plan.composition().reasons.len(),"metrics":plan.composition().metrics}))?;}
            Ok(Box::new(move|m|{let count=plan.composition().stubs.len();let complete=plan.composition().complete();m.plan=Some(plan);m.package=None;m.status=format!("Предпросмотр {} · объектов {} · заглушек {} · исключено {}",if complete{"готов"}else{"неполный"},m.selected.len(),count,m.excluded.len());m.status_kind=if complete{"ok"}else{"error"}.into();m.signals.push(("stubsChanged".into(),vec![m.stubs()]));}))
        })
    }
    fn build(&mut self, callback: Option<u64>) -> Result<()> {
        if self.busy {
            return Err(Error::Invalid(
                "Дождитесь окончания текущей операции".into(),
            ));
        }
        let platform = self
            .platform()
            .ok_or_else(|| Error::Invalid("Выберите установленную платформу 1С".into()))?;
        let source = self
            .source
            .clone()
            .ok_or_else(|| Error::Invalid("Откройте выгрузку".into()))?;
        if self.plan.is_none() {
            return Err(Error::Invalid("Выполните предпросмотр".into()));
        }
        let log = Arc::new(OperationLog::create(
            Path::new(&self.values["logs"]),
            "build",
        )?);
        self.last_log = Some(log.path.clone());
        let plan = self.plan.take().expect("checked preview");
        let excluded = self.excluded.clone();
        let options = BuildOptions {
            destination: PathBuf::from(&self.values["destination"]),
            label: self.values["task"].clone(),
            executable: platform.path,
            mode: mode(&self.preferences.build_mode),
            version: platform.version,
            stamp: chrono::Local::now().format("%Y%m%d_%H%M%S").to_string(),
        };
        self.start(
            "Собираю CF и проверяю обратную выгрузку…",
            callback,
            None,
            move |c, _| {
                let output = Output { log: log.clone() };
                let result = cf_application::build_package(
                    plan.as_ref(),
                    source.as_ref(),
                    &excluded,
                    &options,
                    &cf_application::BuildServices {
                        output: &output,
                        builder: &cf_infrastructure::native::Native { log: log.clone() },
                        log: log.as_ref(),
                    },
                    &c,
                );
                if let Err(e) = &result {
                    let _ = log.failure(&e.to_string());
                }
                let _ = log.value("finished", json!({"success":result.is_ok()}));
                Ok(Box::new(move |m| {
                    m.plan = Some(plan);
                    match result {
                        Ok(folder) => {
                            m.package = Some(folder);
                            m.status =
                                "CF собрана и проверена. В комплекте CF и настройки объединения."
                                    .into();
                            m.status_kind = "ok".into();
                        }
                        Err(e) => {
                            m.status = e.to_string();
                            m.status_kind = "error".into();
                            m.notice(
                                "Ошибка сборки",
                                format!("{e}\n\nПодробности: «Открыть журнал»."),
                            );
                        }
                    }
                }))
            },
        )
    }
    fn pick(&mut self, picker: String, callback: Option<u64>) -> Result<()> {
        let (key, file) = match picker.as_str() {
            "cf-source" => ("export", None),
            "Configuration.xml" => ("export", Some("Configuration.xml")),
            "cf-result" => ("destination", None),
            "cf-logs" => ("logs", None),
            "cf-index" => ("index_directory", None),
            "cf-platform" => (
                if self.preferences.build_mode == "ibcmd" {
                    "platform"
                } else {
                    "designer"
                },
                Some(if self.preferences.build_mode == "ibcmd" {
                    "ibcmd.exe"
                } else {
                    "1cv8.exe"
                }),
            ),
            _ => return Err(Error::Invalid("Неизвестный выбор пути".into())),
        };
        let key = key.to_owned();
        let mut initial = PathBuf::from(&self.values[&key]);
        if initial.is_file() || file.is_some() && !initial.is_dir() {
            initial.pop();
        }
        if initial.as_os_str().is_empty() {
            initial = self
                .preferences
                .last_dirs
                .get(&key)
                .map(PathBuf::from)
                .unwrap_or_else(settings::base);
        }
        while !initial.is_dir() && initial.pop() {}
        self.start(
            "Выберите путь…",
            callback,
            None,
            move |_, _| {
                let dialog = rfd::FileDialog::new().set_directory(initial);
                let chosen = match file {
                    Some("Configuration.xml") => {
                        dialog.add_filter("Configuration.xml", &["xml"]).pick_file()
                    }
                    Some(_) => dialog
                        .add_filter("Программа платформы", &["exe"])
                        .pick_file(),
                    None => dialog.pick_folder(),
                };
                let platform = chosen
                    .as_ref()
                    .filter(|_| key == "platform" || key == "designer")
                    .map(|p| {
                        platform::validate(
                            p,
                            if key == "platform" {
                                BuildMode::Ibcmd
                            } else {
                                BuildMode::Designer
                            },
                        )
                    })
                    .transpose()?;
                Ok(Box::new(move |m| {
                    if let Some(path) = chosen {
                        m.values.insert(key.clone(), path.to_string_lossy().into());
                        let last = if file.is_some() {
                            path.parent().unwrap_or(&path)
                        } else {
                            &path
                        };
                        m.preferences
                            .last_dirs
                            .insert(key.clone(), last.to_string_lossy().into());
                        m.save_soon();
                        if let Some(p) = platform {
                            m.platforms.push(p);
                        }
                        if key == "logs" {
                            journal::set_crash_folder(Path::new(&m.values["logs"]));
                        }
                        if key == "export" {
                            let _ = m.open(None);
                        } else if key == "index_directory"
                            && let Some(s) = &m.source
                        {
                            let identity = s
                                .manifest()
                                .map_or(s.identity(), |v| v.source_key.as_str())
                                .to_owned();
                            m.preferences
                                .index_directories
                                .insert(identity, path.to_string_lossy().into());
                            m.check_index();
                        }
                    }
                }))
            },
        )
    }
    fn check_index(&mut self) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let folder = self.values["index_directory"].clone();
        self.index = None;
        self.invalidate_plan();
        let _ = self.start(
            "Проверяю индекс…",
            None,
            None,
            move |_, _| {
                let status = indexing::status(source.as_ref(), Path::new(&folder))?;
                Ok(Box::new(move |m| {
                    m.index = Some(status);
                    m.status = "Папка индекса выбрана.".into();
                    m.status_kind = "idle".into();
                }))
            },
        );
    }
    fn connect(&mut self, args: &Value, callback: Option<u64>) -> Result<()> {
        let connection: Connection = serde_json::from_value(args[0].clone())
            .map_err(|_| Error::Invalid("Неверные настройки подключения".into()))?;
        let connection = connection.checked()?;
        let target = vault::target(connection.provider()?, &connection.server)?;
        let token = if field(args, 1).is_empty() {
            vault::load(&target)?
                .ok_or_else(|| Error::Invalid("Введите персональный токен".into()))?
        } else {
            Zeroizing::new(field(args, 1).into())
        };
        let remember = args[2].as_bool().unwrap_or(false);
        let folder = self.values["index_directory"].clone();
        let directories = self.preferences.index_directories.clone();
        self.start("Подключаюсь к Git…",callback,Some("git-connect"),move|c,log|{
            let client=git::factory(&connection,token.clone())?;let branches=client.branches(&c)?;
            let source=Arc::new(GitSource::open(&connection,client.clone(),&c)?);
            let folder=directories.get(source.preference_key()).cloned().unwrap_or(folder);
            let index=indexing::status(source.as_ref(),Path::new(&folder))?;
            if remember {vault::save(&target,token.as_str())?;}
            if let Some(log)=log{log.value("connected",json!({"provider":connection.provider,"repository":connection.repository,"branch":connection.branch,"revision":source.revision(),"remembered":remember}))?;}
            Ok(Box::new(move|m|{m.client=Some(client);m.branches=branches;m.preferences.git_settings=connection;m.preferences.source_mode="git".into();m.install_source(source.clone(),Some(source),index,folder);}))
        })
    }
    pub fn rpc(&mut self, method: &str, args: &Value, id: Option<u64>) -> Result<Option<Value>> {
        match method {
            "snapshot"=>return Ok(Some(self.snapshot())),
            "groups"=>{let groups=self.filtered(field(args,0),args[1].as_bool().unwrap_or(false));return Ok(Some(json!(self.labels.iter().filter_map(|(k,_)|groups.get(k).map(|r|json!({"kind":k,"label":self.group_label(k),"count":r.len()}))).collect::<Vec<_>>())));}
            "rows"=>{let groups=self.filtered(field(args,1),args[3].as_bool().unwrap_or(false));let rows=groups.get(field(args,0)).map(|r|r.iter().skip(args[2].as_u64().unwrap_or(0) as usize).take(150).map(|k|self.object(k)).collect::<Vec<_>>()).unwrap_or_default();return Ok(Some(json!(rows)));}
            "gitInfo"=>return Ok(Some(json!(self.preferences.git_settings))),
            "markObject" if !self.busy=>{
                let key=field(args,0);if !self.source.as_ref().is_some_and(|s|s.catalog().objects.keys().any(|k|k.0==key&&k.kind()!="Language")){return Err(Error::Invalid("Неизвестный объект".into()));}
                if args[1]==true{self.selected.insert(key.into());}else{self.selected.remove(key);}
                self.invalidate_plan();self.status="Выбор изменён. Выполните предпросмотр.".into();self.status_kind="idle".into();
            }
            "markStub" if !self.busy=>{
                let key=field(args,0);let mut excluded=self.excluded.clone();if args[1]==true{excluded.remove(key);}else{excluded.insert(key.into());}
                self.plan.as_ref().ok_or_else(||Error::Invalid("Выполните предпросмотр".into()))?.validate_exclusions(&excluded)?;
                self.excluded=excluded;self.package=None;
            }
            "editField" if !self.busy=>{
                let name=field(args,0);let value=field(args,1).to_owned();
                if name=="build_mode"&&["auto","designer","ibcmd"].contains(&value.as_str()){self.preferences.build_mode=value;}
                else if name=="platform_path"{let key=if self.preferences.build_mode=="ibcmd"{"platform"}else{"designer"};self.values.insert(key.into(),value);self.platform_check=true;}
                else if ["export","destination","logs","task","index_directory"].contains(&name){
                    let changed=self.values.get(name)!=Some(&value);
                    self.values.insert(name.into(),value);
                    if name=="logs"{journal::set_crash_folder(Path::new(&self.values["logs"]));}
                    if name=="export"&&changed{self.invalidate_source();self.status="Путь изменён. Откройте XML-выгрузку.".into();self.status_kind="idle".into();}
                    if name=="index_directory"{
                        if let Some(source)=&self.source{let key=source.manifest().map_or(source.identity(),|m|m.source_key.as_str()).to_owned();self.preferences.index_directories.insert(key,self.values["index_directory"].clone());}
                        self.check_index();
                    }
                }
                else{return Err(Error::Invalid("Неизвестное поле".into()));}self.save_soon();
            }
            "setSourceMode" if !self.busy=>{
                let value=field(args,0);if !["git","local"].contains(&value){return Err(Error::Invalid("Неизвестный источник".into()));}
                if self.preferences.source_mode!=value {self.invalidate_source();self.preferences.source_mode=value.into();self.save_soon();self.status="Откройте выбранный источник.".into();self.status_kind="idle".into();}
            }
            "setGitBranch" if !self.busy=>{
                let value=field(args,0);git::validate_branch(value)?;
                if !self.branches.iter().any(|b|b==value){return Err(Error::Invalid("Ветка отсутствует в списке подключения".into()));}
                self.preferences.git_settings.branch=value.into();self.task_filter=None;self.task_revision+=1;self.invalidate_plan();self.save_soon();
                self.open(id)?;return Ok(None);
            }
            "pickPath"=>{self.pick(field(args,0).into(),id)?;return Ok(None);}
            "connectGit"=>{self.connect(args,id)?;return Ok(None);}
            "deleteToken" if !self.busy=>{
                let provider=match field(args,0){"github"=>cf_domain::Provider::Github,"gitea"=>cf_domain::Provider::Gitea,_=>return Err(Error::Invalid("Неизвестный API Git".into()))};
                let target=vault::target(provider,field(args,1))?;vault::delete(&target)?;
                let active=vault::target(self.preferences.git_settings.provider()?,&self.preferences.git_settings.server).ok();
                if active.as_deref()==Some(&target){self.client=None;if self.git.is_some(){self.invalidate_source();}}
                self.notice("Токен удалён","Сохранённый токен выбранного сервера удалён с этого компьютера.");
            }
            "searchGitTask"=>{
                let source=self.git.clone().ok_or_else(||Error::Invalid("Подключитесь к Git".into()))?;
                if self.preferences.source_mode!="git"||source.connection().branch!=self.preferences.git_settings.branch{return Err(Error::Invalid("Откройте выбранную ветку Git".into()));}
                let days=if args[1]==true{None}else{Some(field(args,2).parse::<u32>().ok().filter(|&n|n>0).ok_or_else(||Error::Invalid("Укажите положительное целое количество дней".into()))?)};
                let key=cf_infrastructure::git_source::normalize_commit_query(field(args,0))?.to_owned();
                let proxy=self.proxy.clone();self.task_scope=key.clone();
                self.start("Ищу коммиты по комментарию…",id,Some("git-task"),move|c,log|{
                    let result=source.search(&key,days,&c,&|count|{let _=proxy.send_event(Event::Progress("gitTaskProgress".into(),json!(count)));})?;
                    if let Some(log)=log{log.value("task-filter",json!({"query":key,"days":days,"objects":result.objects,"commits":result.commits,"unmapped":result.missing}))?;}
                    Ok(Box::new(move|m|{m.task_filter=Some(result);m.task_revision+=1;m.task_scope.clear();m.status="Отбор по комментарию готов. Проверьте объекты и нажмите «Отметить найденные».".into();m.status_kind="ok".into();}))
                })?;return Ok(None);
            }
            "openLink"=>{let url=match field(args,0){"repository"=>"https://github.com/alonehobo/1c-partial-cf","licenses"=>"https://github.com/tauri-apps/wry","icons"=>"https://lucide.dev/license",_=>return Err(Error::Invalid("Неизвестная ссылка".into()))};shell_open(Path::new(url))?;}
            "command"=>{
                match field(args,0){
                    "open-source"=>{
                        if self.preferences.source_mode=="git"&&self.client.is_none(){self.signals.push(("connectRequested".into(),vec![]));}
                        else{self.open(id)?;return Ok(None);}
                    }
                    "refresh-index"=>{self.refresh(id)?;return Ok(None);}
                    "preview"=>{self.preview(id)?;return Ok(None);}
                    "build"=>{self.build(id)?;return Ok(None);}
                    "logs"=>{let path=Path::new(&self.values["logs"]);std::fs::create_dir_all(path).map_err(cf_infrastructure::external)?;shell_open(path)?;}
                    "package"=>{if let Some(path)=&self.package{shell_open(path)?;}}
                    "journal"=>{if let Some(path)=self.last_log.clone(){self.start("Читаю журнал…",id,None,move|_,_|{let text=journal::read(&path)?;Ok(Box::new(move|m|m.signals.push(("journalOpened".into(),vec![json!(path),json!(text)]))))})?;return Ok(None);}}
                    "about"=>self.notice("О программе",format!("Мастер технической CF · {}\nПолностью на Rust: WebView2, SQLite.\n\nАлгоритм подготовки технической конфигурации перенесён из partial_cf.py проекта alonehobo/1c-partial-cf. Большое спасибо alonehobo за исходный скрипт.\n\nВнешние команды установленной 1С выполняют сборку CF и обратную проверку.",env!("CARGO_PKG_VERSION"))),
                    "cancel-git-task"=>{self.cancel.cancel();self.cancelling=true;}
                    "mark-git-task" if !self.busy=>{if let Some(result)=&self.task_filter{self.selected.extend(result.objects.iter().cloned());self.invalidate_plan();}}
                    "clear-git-task" if !self.busy=>{self.task_filter=None;self.task_revision+=1;}
                    _=>return Err(Error::Invalid("Неизвестная или недоступная команда".into()))
                }
            }
            _=>return Err(Error::Invalid("Неизвестный или недоступный вызов интерфейса".into())),
        }
        Ok(Some(self.snapshot()))
    }
    pub fn tick(&mut self) {
        if self
            .dirty
            .is_some_and(|t| t.elapsed() > Duration::from_millis(400))
        {
            self.flush();
        }
        if !self.busy
            && !self.polling
            && self.preferences.source_mode == "git"
            && self.preferences.git_settings.interval != 0
            && Instant::now() >= self.next_poll
            && let Some(source) = self.git.clone()
        {
            let proxy = self.proxy.clone();
            let generation = self.generation;
            self.polling = true;
            self.next_poll =
                Instant::now() + Duration::from_secs(self.preferences.git_settings.interval as u64);
            std::thread::spawn(move || {
                let result = source
                    .client()
                    .head(&source.connection().branch, &Cancellation::default());
                let _ = proxy.send_event(Event::Poll(generation, result));
            });
        }
    }
    pub fn apply(&mut self, generation: u64, outcome: Result<Update>) {
        if generation != self.generation {
            return;
        }
        self.busy = false;
        self.cancelling = false;
        match outcome {
            Ok(update) => update(self),
            Err(error) => {
                if self.preferences.source_mode == "git"
                    && let Some(git) = &self.git
                {
                    self.preferences.git_settings.branch = git.connection().branch.clone();
                    self.save_soon();
                }
                self.task_scope.clear();
                self.status = error.to_string();
                self.status_kind = "error".into();
                self.notice(
                    "Мастер технической CF",
                    format!("{error}\n\nПодробности доступны в журнале операции."),
                );
            }
        }
    }
}
#[cfg(windows)]
pub fn shell_open(path: &Path) -> Result<()> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            op: *const u16,
            file: *const u16,
            args: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
    let file = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result <= 32 {
        return Err(Error::External(
            "Не удалось открыть ссылку или папку".into(),
        ));
    }
    Ok(())
}
#[cfg(not(windows))]
pub fn shell_open(_: &Path) -> Result<()> {
    Err(Error::Invalid("Внешнее открытие доступно в Windows".into()))
}

#[cfg(test)]
mod stub_group_tests {
    use super::*;
    use cf_domain::{Cause, Stub};

    fn stub(key: &str, owners: &[&str]) -> (String, Stub) {
        (
            key.into(),
            Stub {
                key: key.into(),
                causes: owners
                    .iter()
                    .map(|owner| Cause {
                        owner: (*owner).into(),
                        required: true,
                    })
                    .collect(),
            },
        )
    }

    #[test]
    fn groups_dependencies_by_their_own_type_instead_of_owner_type() {
        let stubs = BTreeMap::from([
            stub("Catalog.Items", &["CommonModule.Exchange"]),
            stub("Document.Order", &["InformationRegister.Queue"]),
        ]);
        let groups = stub_groups(&stubs);
        assert_eq!(
            groups.keys().map(String::as_str).collect::<Vec<_>>(),
            ["Catalog", "Document"]
        );
        assert_eq!(
            groups["Catalog"]["Catalog.Items"],
            ["CommonModule.Exchange"]
        );
        assert!(!groups.contains_key("CommonModule"));
        assert!(!groups.contains_key("InformationRegister"));
    }

    #[test]
    fn each_stub_has_one_row_while_retaining_every_distinct_cause() {
        let stubs = BTreeMap::from([
            stub(
                "Catalog.Items",
                &[
                    "InformationRegister.Queue",
                    "CommonModule.Exchange",
                    "CommonModule.Exchange",
                ],
            ),
            stub("Document.Order", &[]),
        ]);
        let groups = stub_groups(&stubs);
        assert_eq!(
            groups.values().map(BTreeMap::len).sum::<usize>(),
            stubs.len()
        );
        assert_eq!(
            groups["Catalog"]["Catalog.Items"],
            ["CommonModule.Exchange", "InformationRegister.Queue"]
        );
        assert!(groups["Document"]["Document.Order"].is_empty());
    }

    #[test]
    fn phase_logging_keeps_the_original_operation_error() {
        let result = preserve_operation_error::<()>(
            Err(Error::Invalid("index failure".into())),
            Err(Error::External("log failure".into())),
        );
        assert!(matches!(result, Err(Error::Invalid(message)) if message == "index failure"));
    }

    #[test]
    fn phase_logging_preserves_success_and_reports_log_failure_after_success() {
        assert_eq!(preserve_operation_error(Ok(7), Ok(())).unwrap(), 7);
        let result = preserve_operation_error(Ok(7), Err(Error::External("log failure".into())));
        assert!(matches!(result, Err(Error::External(message)) if message == "log failure"));
    }
}
