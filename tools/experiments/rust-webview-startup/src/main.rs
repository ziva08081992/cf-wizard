#![windows_subsystem = "windows"]
use std::{env, fs, path::PathBuf, sync::{Arc,atomic::{AtomicBool,Ordering}}, time::{Duration,Instant}};
use tao::{dpi::LogicalSize,event::{Event,WindowEvent},event_loop::{ControlFlow,EventLoop},window::WindowBuilder};
use wry::{PageLoadEvent,WebContext,WebViewBuilder};
fn main() {
    if let Err(error)=run() {
        if let Some(path)=env::args().nth(1) { let _=fs::write(PathBuf::from(path).with_extension("error.txt"),format!("{error:?}")); }
        std::process::exit(1);
    }
}
fn run()->Result<(),Box<dyn std::error::Error>> {
    let args:Vec<String>=env::args().collect();
    if args.len()!=3 { return Err("Expected report and isolated profile paths".into()); }
    let report=PathBuf::from(&args[1]);let profile=PathBuf::from(&args[2]);fs::create_dir_all(&profile)?;
    let events=EventLoop::new();
    let window=WindowBuilder::new().with_title("CF Wizard — Rust/WebView2 startup probe")
        .with_inner_size(LogicalSize::new(1180.,780.)).build(&events)?;
    let loaded=Arc::new(AtomicBool::new(false));let loaded_signal=loaded.clone();
    let evaluating=Arc::new(AtomicBool::new(false));
    let mut context=WebContext::new(Some(profile));
    let webview=WebViewBuilder::new_with_web_context(&mut context).with_devtools(false)
        .with_navigation_handler(|url|url=="about:blank" || url.starts_with("http://cfprobe.localhost/") || url.starts_with("cfprobe://"))
        .with_on_page_load_handler(move |event,_uri| {if matches!(event,PageLoadEvent::Finished) {loaded_signal.store(true,Ordering::Release);}})
        .with_custom_protocol("cfprobe".into(), |_id, _request| {
            wry::http::Response::builder().header("Content-Type","text/html; charset=utf-8")
                .body(std::borrow::Cow::Borrowed(&include_bytes!("../ui.html")[..])).unwrap()
        })
        .with_url("cfprobe://localhost/").build(&window)?;
    events.run(move |event,_,control| {
        *control=ControlFlow::WaitUntil(Instant::now()+Duration::from_millis(10));
        if report.exists() { *control=ControlFlow::Exit; return; }
        if loaded.load(Ordering::Acquire) && !evaluating.swap(true,Ordering::AcqRel) {
            let destination=report.clone();let done=evaluating.clone();
            let _=webview.evaluate_script_with_callback(r#"(() => {
              const element=document.querySelector('.cf-window');
              return {webview_loaded:!!element && document.body.innerText.length>100 && document.querySelectorAll("button").length>=10,
                ready_state:document.readyState,text_length:document.body.innerText.length,buttons:document.querySelectorAll("button").length,
                background:element?getComputedStyle(element).backgroundColor:'missing',title:document.title};
            })()"#,move|result| {
                if result.contains("\"webview_loaded\":true") {let _=fs::write(&destination,&result);}
                else {let _=fs::write(destination.with_extension("dom.txt"),&result);}
                done.store(false,Ordering::Release);
            });
        }
        if let Event::WindowEvent {event:WindowEvent::CloseRequested,..}=event { *control=ControlFlow::Exit; }
    });
}
