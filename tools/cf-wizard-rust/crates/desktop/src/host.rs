//! Native WebView host. Local resources and typed, origin-checked IPC only.
use crate::model::{Event, Model};
use cf_domain::{BuildMode, Error, Result};
use cf_infrastructure::{platform, settings};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event as TaoEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{WebContext, WebView, WebViewBuilder};

fn emit(view: &WebView, name: &str, args: Vec<Value>) {
    let code = format!(
        "window.__cfNative?.signal({},{});",
        json!(name),
        json!(args)
    );
    let _ = view.evaluate_script(&code);
}
fn reply(view: &WebView, id: u64, result: Value) {
    let _ = view.evaluate_script(&format!("window.__cfNative?.reply({id},{});", result));
}
fn paint(view: &WebView, model: &mut Model) {
    emit(view, "changed", vec![model.snapshot()]);
    for (name, args) in std::mem::take(&mut model.signals) {
        emit(view, &name, args);
    }
    for (title, text) in std::mem::take(&mut model.notices) {
        emit(view, "notice", vec![json!(title), json!(text)]);
    }
}
fn resource(path: &str) -> Option<(&'static str, &'static [u8])> {
    Some(match path {
        "/" | "/index.html" => (
            "text/html; charset=utf-8",
            include_bytes!("../ui/index.html"),
        ),
        "/theme.css" => ("text/css; charset=utf-8", include_bytes!("../ui/theme.css")),
        "/icons.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/icons.js"),
        ),
        "/bridge.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/bridge.js"),
        ),
        "/app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/app.js"),
        ),
        _ => return None,
    })
}
fn trusted(uri: &wry::http::Uri) -> bool {
    (uri.scheme_str() == Some("http") && uri.host() == Some("cfwizard.localhost"))
        || (uri.scheme_str() == Some("cfwizard") && uri.host() == Some("localhost"))
}
pub fn data_directory() -> PathBuf {
    let args = std::env::args().collect::<Vec<_>>();
    args.iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(settings::base)
}
pub fn run() -> Result<()> {
    let started = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let option = |key: &str| {
        args.iter()
            .position(|a| a == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if args.iter().any(|a| a == "--native-check") {
        return native_check(&args);
    }
    let base = data_directory();
    fs::create_dir_all(&base).map_err(cf_infrastructure::external)?;
    let events = EventLoopBuilder::<Event>::with_user_event().build();
    let proxy = events.create_proxy();
    let mut model = Model::new(base.join("paths.json"), proxy.clone());
    if let Some(path) = option("--export") {
        model.values.insert("export".into(), path);
        model.preferences.source_mode = "local".into();
    }
    let report = option("--smoke-report").map(PathBuf::from);
    let mut smoke_requested = false;
    let exercise = args.iter().any(|a| a == "--exercise-ui");
    let mut ready_seconds = None;
    let mut ready = false;
    let profile = base.join("webview-rust");
    fs::create_dir_all(&profile).map_err(cf_infrastructure::external)?;
    let window = WindowBuilder::new()
        .with_title("Мастер технической CF · Rust")
        .with_inner_size(LogicalSize::new(1180., 820.))
        .with_min_inner_size(LogicalSize::new(860., 560.))
        .build(&events)
        .map_err(cf_infrastructure::external)?;
    let mut context = WebContext::new(Some(profile));
    let request_proxy = proxy.clone();
    let view = WebViewBuilder::new_with_web_context(&mut context)
        .with_devtools(false)
        .with_background_color((30, 31, 34, 255))
        .with_navigation_handler(|address| {
            address == "about:blank"
                || address == "http://cfwizard.localhost/"
                || address == "http://cfwizard.localhost/index.html"
                || address == "cfwizard://localhost/"
                || address == "cfwizard://localhost/index.html"
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_custom_protocol("cfwizard".into(), |_, request| {
            if let Some((mime, data)) = resource(request.uri().path()) {
                wry::http::Response::builder()
                    .header("Content-Type", mime)
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Cow::Borrowed(data))
                    .unwrap()
            } else {
                wry::http::Response::builder()
                    .status(404)
                    .body(Cow::Borrowed(&b"Not found"[..]))
                    .unwrap()
            }
        })
        .with_ipc_handler(move |request| {
            if !trusted(request.uri()) || request.body().len() > 1024 * 1024 {
                return;
            }
            let mut body = request.into_body();
            let parsed = serde_json::from_str::<Value>(&body);
            use zeroize::Zeroize;
            body.zeroize();
            if let Ok(value) = parsed {
                let _ = request_proxy.send_event(Event::Rpc(value));
            }
        })
        .with_url("cfwizard://localhost/")
        .build(&window)
        .map_err(|e| {
            Error::External(format!(
                "WebView2: {e}. Проверьте установленный Microsoft Edge WebView2 Runtime."
            ))
        })?;
    events.run(move|event,_,control|{
        *control=ControlFlow::WaitUntil(Instant::now()+Duration::from_millis(150));
        match event {
            TaoEvent::UserEvent(Event::Rpc(request))=>{
                let id=request["id"].as_u64();let method=request["method"].as_str().unwrap_or("").to_owned();
                if method=="viewReady"{
                    ready=true;
                    ready_seconds=Some(started.elapsed().as_secs_f64());
                    let manual=[(model.values["designer"].clone(),BuildMode::Designer),(model.values["platform"].clone(),BuildMode::Ibcmd)];
                    let proxy=proxy.clone();std::thread::spawn(move||{let mut p=platform::discover(BuildMode::Designer);p.extend(platform::discover(BuildMode::Ibcmd));for (path,mode) in manual{if !path.is_empty()&& let Ok(value)=platform::validate(std::path::Path::new(&path),mode){p.push(value);}}let _=proxy.send_event(Event::Platforms(p));});
                    if model.preferences.source_mode=="git"&&!model.preferences.git_settings.server.is_empty()&& let Err(e)=model.open(None){model.status=e.to_string();model.status_kind="idle".into();}
                    paint(&view,&mut model);
                } else if method=="smokeResult"&&report.is_some() {
                    if let Some(path)=&report {let mut value=request["args"][0].clone();value["seconds"]=json!(started.elapsed().as_secs_f64());value["readySeconds"]=json!(ready_seconds);value["version"]=json!(env!("CARGO_PKG_VERSION"));let _=fs::write(path,serde_json::to_vec_pretty(&value).unwrap());}
                    *control=ControlFlow::Exit;
                } else if method=="pickCache" {
                    let initial=request["args"][0].as_str().map(PathBuf::from).unwrap_or_else(settings::base);
                    let proxy=proxy.clone();
                    std::thread::spawn(move||{let path=rfd::FileDialog::new().set_directory(initial).pick_folder();let _=proxy.send_event(Event::Progress("pickCache".into(),json!({"id":id,"path":path})));});
                } else {
                    let mut request=request;
                    let mut arguments=request["args"].take();
                    let outcome=model.rpc(&method,&arguments,id);
                    // Minimise lifetime of IPC token copies. No RPC payload is logged.
                    if method=="connectGit"&& let Some(Value::String(token))=arguments.get_mut(1){use zeroize::Zeroize;token.zeroize();}
                    match outcome {Ok(Some(value))=>{if let Some(id)=id{reply(&view,id,value);}},Ok(None)=>{},Err(error)=>{model.status=error.to_string();model.status_kind="error".into();model.notice("Мастер технической CF",error.to_string());if let Some(id)=id{reply(&view,id,model.snapshot());}}}
                    paint(&view,&mut model);
                }
            }
            TaoEvent::UserEvent(Event::Done{generation,callback,outcome,log})=>{
                model.apply(generation,outcome);if let Some(log)=log{let _=log.value("finished",json!({"status":model.status_kind}));}
                if let Some(id)=callback{reply(&view,id,model.snapshot());}paint(&view,&mut model);
            }
            TaoEvent::UserEvent(Event::Progress(kind,value))=>{
                if kind=="gitTaskProgress"{emit(&view,&kind,vec![value]);}
                else if kind=="pickCache"{if let Some(id)=value["id"].as_u64(){reply(&view,id,value["path"].clone());}}
                else if kind=="index"{model.status=format!("Обновление индекса · {} / {}",value["done"],value["total"]);paint(&view,&mut model);}
            }
            TaoEvent::UserEvent(Event::Platforms(rows))=>{model.add_platforms(rows);paint(&view,&mut model);}
            TaoEvent::UserEvent(Event::PlatformChecked(key,path,result))=>{
                if model.values.get(&key)==Some(&path){match result{Ok(p)=>model.add_platforms(vec![p]),Err(e) if !model.busy=>{model.status=format!("Путь платформы: {e}");model.status_kind="error".into();}Err(_)=>{}}paint(&view,&mut model);}
            }
            TaoEvent::UserEvent(Event::Poll(generation,outcome))=>{
                model.polling=false;
                if generation==model.generation {match outcome{Ok(sha)=>{model.poll_info=if model.git.as_ref().is_some_and(|g|g.revision()!=sha){"В ветке есть изменения. Нажмите «Обновить ветку».".into()}else{"SHA ветки проверен · изменений нет".into()};}Err(error)=>model.poll_info=format!("Проверка SHA: {error}")}paint(&view,&mut model);}
            }
            TaoEvent::WindowEvent{event:WindowEvent::CloseRequested,..}=>{
                if model.busy{model.cancel.cancel();model.notice("Операция выполняется","Дождитесь окончания операции. Поиск и сборка получили запрос отмены.");paint(&view,&mut model);}
                else{model.flush();*control=ControlFlow::Exit;}
            }
            TaoEvent::MainEventsCleared=>{
                model.tick();
                if ready&&report.is_some()&&!smoke_requested&&!model.busy&&(!exercise||model.source.is_some()){
                    smoke_requested=true;
                    if exercise{let _=view.evaluate_script(include_str!("../ui/smoke.js"));}
                    else{let _=view.evaluate_script(r#"(() => {
                      const root=document.querySelector('.cf-window');
                      window.__cfNative.call('smokeResult',[{
                        loaded:!!root && document.querySelectorAll('button').length>30,
                        buttons:document.querySelectorAll('button').length,
                        title:document.title,background:root?getComputedStyle(root).backgroundColor:'',
                        qtLoaded:!!document.querySelector('script[src^="qrc:"]'),
                        gitDialog:!!document.querySelector('#cf-git-dialog'),
                        status:document.querySelector('[data-status]').textContent
                      }]);
                    })()"#);}
                }
                if report.is_some()&&started.elapsed()>Duration::from_secs(120){if let Some(path)=&report{let _=fs::write(path,b"{\"loaded\":false,\"error\":\"UI timeout\"}");}*control=ControlFlow::Exit;}
            }
            _=>{}
        }
    });
}
fn native_check(args: &[String]) -> Result<()> {
    use cf_application::{BuildOptions, Cancellation, DependencyEngine, MetadataSource};
    use cf_infrastructure::{
        engine::RustEngine, journal::OperationLog, native::Native, output::Output,
        source::LocalSource,
    };
    use std::{collections::BTreeSet, sync::Arc};
    let arg = |key: &str| {
        args.iter()
            .position(|a| a == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("Не указан {key}")))
    };
    let report = PathBuf::from(arg("--report")?);
    let output = (|| -> Result<Value> {
        let source = LocalSource::open(&PathBuf::from(arg("--export")?))?;
        let selected = arg("--select")?
            .split(',')
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let cancel = Cancellation::default();
        let plan = RustEngine.preview(&source, None, &selected, &BTreeSet::new(), &cancel)?;
        let mode = crate::model::mode(&arg("--mode")?);
        let platform = platform::validate(&PathBuf::from(arg("--platform")?), mode)?;
        let logs = PathBuf::from(arg("--logs")?);
        let log = Arc::new(OperationLog::create(&logs, "native-check")?);
        let options = BuildOptions {
            destination: PathBuf::from(arg("--destination")?),
            label: "Rust-проверка".into(),
            executable: platform.path,
            mode,
            version: platform.version,
            stamp: chrono::Local::now().format("%Y%m%d_%H%M%S").to_string(),
        };
        let started = Instant::now();
        let package = cf_application::build_package(
            plan.as_ref(),
            &source,
            &BTreeSet::new(),
            &options,
            &cf_application::BuildServices {
                output: &Output { log: log.clone() },
                builder: &Native { log: log.clone() },
                log: log.as_ref(),
            },
            &cancel,
        )?;
        Ok(
            json!({"passed":true,"package":package,"seconds":started.elapsed().as_secs_f64(),"configuration":source.catalog().name,"journal":log.path}),
        )
    })();
    let value = match &output {
        Ok(value) => value.clone(),
        Err(e) => json!({"passed":false,"error":e.to_string()}),
    };
    fs::write(
        report,
        serde_json::to_vec_pretty(&value).map_err(cf_infrastructure::external)?,
    )
    .map_err(cf_infrastructure::external)?;
    output.map(|_| ())
}
#[cfg(windows)]
pub fn error_message(text: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, title: *const u16, kind: u32) -> i32;
    }
    let text = text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let title = "Мастер технической CF"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(0, text.as_ptr(), title.as_ptr(), 0x10);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_origin_and_resources_only() {
        assert!(trusted(&"http://cfwizard.localhost/".parse().unwrap()));
        assert!(!trusted(&"https://evil.test/".parse().unwrap()));
        assert!(!trusted(
            &"http://cfwizard.localhost.evil/".parse().unwrap()
        ));
        assert!(resource("/../paths.json").is_none());
        assert!(resource("/qwebchannel.js").is_none());
    }
}
