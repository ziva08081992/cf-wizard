//! Disposable native builds, complete readback and inventory proof.
use crate::{
    canonical::sha256,
    source::{canonical_future, no_links, normal_path, relative_path},
    xml::{Xml, unescape},
};
use cf_application::{Cancellation, NativeArtifact, NativeBuilder, NativeRequest};
use cf_domain::{BuildMode, Error, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct Native;
pub struct Outcome {
    pub code: Option<i32>,
    pub timed_out: bool,
}
pub trait Runner {
    fn run(
        &self,
        executable: &Path,
        args: &[String],
        literal_connection: bool,
        log: &Path,
        cancel: &Cancellation,
    ) -> Result<Outcome>;
}
pub struct ProcessRunner;
impl Runner for ProcessRunner {
    fn run(
        &self,
        executable: &Path,
        args: &[String],
        literal_connection: bool,
        log: &Path,
        cancel: &Cancellation,
    ) -> Result<Outcome> {
        cancel.check()?;
        let output = File::create(log).map_err(crate::external)?;
        let mut cmd = Command::new(executable);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
            if literal_connection {
                cmd.arg(&args[0]).raw_arg(&args[1]).args(&args[2..]);
            } else {
                cmd.args(args);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = literal_connection;
            cmd.args(args);
        }
        cmd.stdin(Stdio::null())
            .stdout(output.try_clone().map_err(crate::external)?)
            .stderr(output);
        let mut child = cmd.spawn().map_err(crate::external)?;
        let started = Instant::now();
        loop {
            if let Some(exit) = child.try_wait().map_err(crate::external)? {
                return Ok(Outcome {
                    code: exit.code(),
                    timed_out: false,
                });
            }
            if cancel.check().is_err() || started.elapsed() > Duration::from_secs(240) {
                let _ = child.kill();
                let _ = child.wait();
                cancel.check()?;
                return Ok(Outcome {
                    code: None,
                    timed_out: true,
                });
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
pub fn inventory(root: &Path, cancel: &Cancellation) -> Result<BTreeMap<String, String>> {
    no_links(root)?;
    let mut todo = vec![root.to_path_buf()];
    let mut out = BTreeMap::new();
    while let Some(dir) = todo.pop() {
        cancel.check()?;
        for entry in fs::read_dir(&dir).map_err(crate::external)? {
            let path = entry.map_err(crate::external)?.path();
            no_links(&path)?;
            let meta = fs::metadata(&path).map_err(crate::external)?;
            if meta.is_dir() {
                todo.push(path);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(crate::external)?
                    .to_string_lossy()
                    .replace('\\', "/");
                relative_path(&rel)?;
                let mut file = File::open(&path).map_err(crate::external)?;
                let mut hash = sha2::Sha256::default();
                use sha2::Digest;
                let mut buf = [0u8; 65536];
                loop {
                    cancel.check()?;
                    let len = file.read(&mut buf).map_err(crate::external)?;
                    if len == 0 {
                        break;
                    }
                    hash.update(&buf[..len]);
                }
                out.insert(rel, hex::encode(hash.finalize()));
            } else {
                return Err(Error::Invalid("Недопустимый файл в рабочей папке".into()));
            }
        }
    }
    Ok(out)
}
pub fn verify_inventory(
    root: &Path,
    expected: &BTreeMap<String, String>,
    cancel: &Cancellation,
) -> Result<()> {
    if expected.is_empty() || &inventory(root, cancel)? != expected {
        return Err(Error::Invalid(
            "Изменился состав или содержимое staging".into(),
        ));
    }
    Ok(())
}
fn expanded(name: &str, scope: &BTreeMap<String, String>, attribute: bool) -> Result<String> {
    if let Some((p, n)) = name.split_once(':') {
        let uri = scope
            .get(p)
            .ok_or_else(|| Error::Invalid("Необъявленный XML namespace".into()))?;
        Ok(format!("{{{uri}}}{n}"))
    } else if !attribute && scope.get("").is_some_and(|uri| !uri.is_empty()) {
        Ok(format!("{{{}}}{name}", scope[""]))
    } else {
        Ok(name.into())
    }
}
fn qvalue(value: &str, scope: &BTreeMap<String, String>, preserve: bool) -> Value {
    if !preserve && value.trim_matches([' ', '\t', '\n', '\r']).is_empty() {
        return Value::Null;
    }
    if let Some((p, n)) = value.split_once(':') {
        fn qname(s: &str) -> bool {
            let mut ch = s.chars();
            ch.next().is_some_and(|c| c == '_' || c.is_alphabetic())
                && ch.all(|c| c == '_' || c.is_alphanumeric() || matches!(c, '.' | '-'))
        }
        if qname(p)
            && qname(n)
            && let Some(uri) = scope.get(p)
        {
            return json!(["qname", uri, n]);
        }
    }
    json!(["text", value])
}
/// Match ElementTree expanded names, QName values and inherited xml:space.
pub fn xml_signature(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(crate::external)?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let x = Xml::parse(text.as_bytes())?;
    fn node(
        x: &Xml,
        id: usize,
        parent: &BTreeMap<String, String>,
        inherited: bool,
    ) -> Result<Value> {
        let e = &x.elements[id];
        let mut scope = parent.clone();
        for a in &e.attributes {
            if a.name == "xmlns" {
                scope.insert("".into(), a.value.clone());
            } else if let Some(p) = a.name.strip_prefix("xmlns:") {
                scope.insert(p.into(), a.value.clone());
            }
        }
        let preserve = e
            .attributes
            .iter()
            .find(|a| a.name == "xml:space")
            .map_or(inherited, |a| a.value == "preserve");
        let mut attrs = BTreeMap::new();
        for a in &e.attributes {
            if a.name == "xmlns" || a.name.starts_with("xmlns:") {
                continue;
            }
            let value = unescape(&a.raw.replace(['\t', '\n', '\r'], " "))?;
            attrs.insert(
                expanded(&a.name, &scope, true)?,
                qvalue(&value, &scope, true),
            );
        }
        let mut text_ranges = e.text.iter().peekable();
        let mut segment = |start: usize, end: usize| -> Result<Value> {
            let mut value = String::new();
            while text_ranges.peek().is_some_and(|(r, _)| r.end <= end) {
                let (r, cdata) = text_ranges.next().expect("peeked text");
                if r.start >= start {
                    let t = &x.source[r.clone()];
                    value.push_str(&if *cdata { t.into() } else { unescape(t)? });
                }
            }
            Ok(qvalue(&value, &scope, preserve))
        };
        let head = segment(
            e.inner.start,
            e.children
                .first()
                .map_or(e.inner.end, |&c| x.elements[c].span.start),
        )?;
        let mut children = vec![];
        for (i, &c) in e.children.iter().enumerate() {
            let tail = segment(
                x.elements[c].span.end,
                e.children
                    .get(i + 1)
                    .map_or(e.inner.end, |&n| x.elements[n].span.start),
            )?;
            children.push(json!([node(x, c, &scope, preserve)?, tail]));
        }
        Ok(json!([
            expanded(&e.name, &scope, false)?,
            attrs,
            head,
            children
        ]))
    }
    node(
        &x,
        x.root,
        &BTreeMap::from([("xml".into(), "http://www.w3.org/XML/1998/namespace".into())]),
        false,
    )
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(crate::external)?,
    )
    .map_err(crate::external)
}
fn log_text(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(&bytes)
        .trim_start_matches('\u{feff}')
        .to_owned()
}
impl NativeBuilder for Native {
    fn build(&self, request: &NativeRequest, cancel: &Cancellation) -> Result<NativeArtifact> {
        build_with(request, &ProcessRunner, cancel)
    }
}
pub fn build_with(
    r: &NativeRequest,
    runner: &dyn Runner,
    cancel: &Cancellation,
) -> Result<NativeArtifact> {
    let stage = canonical_future(&r.staging)?;
    let work = canonical_future(&r.work)?;
    let raw = normal_path(&work);
    if raw.starts_with(r"\\")
        || work.exists()
        || work.starts_with(&stage)
        || stage.starts_with(&work)
        || work
            .components()
            .any(|p| p.as_os_str().eq_ignore_ascii_case(".git"))
        || !work.parent().is_some_and(Path::is_dir)
    {
        return Err(Error::Invalid(
            "Для сборки нужна новая отдельная локальная рабочая папка".into(),
        ));
    }
    relative_path(&r.configuration)?;
    relative_path(&r.stamp)?;
    if r.configuration.contains('/')
        || r.stamp.contains('/')
        || !r
            .stamp
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::Invalid("Неверное имя выходного CF".into()));
    }
    let platform = crate::platform::validate(&r.executable, r.mode)?;
    let platform_hash = sha256(&fs::read(&r.executable).map_err(crate::external)?);
    if platform.version != r.platform_version {
        return Err(Error::Invalid("Версия платформы изменилась".into()));
    }
    verify_inventory(&stage, &r.inventory, cancel)?;
    let designer = r.mode != BuildMode::Ibcmd;
    fs::create_dir(&work).map_err(crate::external)?;
    let logs = work.join("logs");
    fs::create_dir(&logs).map_err(crate::external)?;
    let output = work.join("output");
    fs::create_dir(&output).map_err(crate::external)?;
    let cf = output.join(format!("{}_{}.cf", r.configuration, r.stamp));
    let db = work.join("db");
    let data = work.join("data");
    let checkdb = work.join("readback-db");
    let back = work.join("readback");
    let now = Instant::now();
    let mut receipt = json!({"status":"running","backend":platform.backend,"platform":platform.version,"stageFiles":r.inventory.len(),"steps":[],"checks":[],"runtimeAcceptance":false,"existingDatabasesChanged":false,"updateDBCfg":false,"startedUtc":chrono::Utc::now().to_rfc3339()});
    let path = |p: &Path| normal_path_display(p);
    let mut invoke = |name: &str, mut args: Vec<String>| -> Result<()> {
        cancel.check()?;
        let clock = Instant::now();
        let log = logs.join(format!("{name}.log"));
        let out = logs.join(format!("{name}.out"));
        let result = logs.join(format!("{name}.result"));
        let literal = designer && args[0] == "CREATEINFOBASE";
        if designer {
            args.extend([
                "/Out".into(),
                path(&out),
                "/DumpResult".into(),
                path(&result),
            ]);
        }
        let outcome = runner.run(&r.executable, &args, literal, &log, cancel);
        let result_code = if designer {
            log_text(&result).trim().to_owned()
        } else {
            String::new()
        };
        let (code, timeout, error) = match outcome {
            Ok(o) => (o.code, o.timed_out, None),
            Err(e) => (None, false, Some(e.to_string())),
        };
        receipt["steps"].as_array_mut().unwrap().push(json!({"step":name,"args":args,"seconds":clock.elapsed().as_secs_f64(),"exitCode":code,"resultCode":result_code,"timedOut":timeout,"error":error,"log":log}));
        write_json(&work.join("build-progress.json"), &receipt)?;
        if let Some(e) = error {
            return Err(Error::External(e));
        }
        if timeout || code != Some(0) || designer && result_code != "0" {
            return Err(Error::External(format!(
                "Ошибка {name}: код {code:?}, DumpResult={result_code}; журнал {}",
                log.display()
            )));
        }
        Ok(())
    };
    let create = |name: &str,
                  dir: &Path,
                  invoke: &mut dyn FnMut(&str, Vec<String>) -> Result<()>|
     -> Result<()> {
        invoke(
            name,
            vec![
                "CREATEINFOBASE".into(),
                format!("File=\"{}\";", path(dir)),
                "/DisableStartupDialogs".into(),
            ],
        )?;
        if !dir.join("1Cv8.1CD").is_file() {
            return Err(Error::External(
                "Платформа не создала временную базу".into(),
            ));
        }
        Ok(())
    };
    let designer_args = |dir: &Path, extra: Vec<String>| {
        let mut v = vec![
            "DESIGNER".into(),
            "/F".into(),
            path(dir),
            "/DisableStartupDialogs".into(),
            "/DisableStartupMessages".into(),
            "/DisableSplash".into(),
            "/DisableBackgroundIndexBuild".into(),
        ];
        v.extend(extra);
        v
    };
    let result = (|| -> Result<()> {
        if designer {
            create("01-create", &db, &mut invoke)?;
            invoke(
                "02-load-xml",
                designer_args(&db, vec!["/LoadConfigFromFiles".into(), path(&stage)]),
            )?;
            invoke(
                "03-save-cf",
                designer_args(&db, vec!["/DumpCfg".into(), path(&cf)]),
            )?;
            if !cf.is_file() || fs::metadata(&cf).map_err(crate::external)?.len() == 0 {
                return Err(Error::External("Платформа не создала CF".into()));
            }
            create("04-create-readback", &checkdb, &mut invoke)?;
            invoke(
                "05-load-cf",
                designer_args(&checkdb, vec!["/LoadCfg".into(), path(&cf)]),
            )?;
            fs::create_dir(&back).map_err(crate::external)?;
            invoke(
                "06-export-xml",
                designer_args(
                    &checkdb,
                    vec![
                        "/DumpConfigToFiles".into(),
                        path(&back),
                        "-Format".into(),
                        "Hierarchical".into(),
                    ],
                ),
            )?;
        } else {
            invoke(
                "01-create",
                vec![
                    "infobase".into(),
                    "create".into(),
                    format!("--db-path={}", path(&db)),
                    format!("--data={}", path(&data)),
                ],
            )?;
            invoke(
                "02-import",
                vec![
                    "config".into(),
                    "import".into(),
                    format!("--db-path={}", path(&db)),
                    format!("--data={}", path(&data)),
                    format!("--out={}", path(&cf)),
                    path(&stage),
                ],
            )?;
            invoke(
                "03-export",
                vec![
                    "config".into(),
                    "export".into(),
                    format!("--db-path={}", path(&db)),
                    format!("--data={}", path(&data)),
                    format!("--file={}", path(&cf)),
                    path(&back),
                ],
            )?;
        }
        if !cf.is_file() || fs::metadata(&cf).map_err(crate::external)?.len() == 0 {
            return Err(Error::External("Нет непустого CF".into()));
        }
        no_links(&cf)?;
        no_links(&back)?;
        for rel in r.inventory.keys() {
            cancel.check()?;
            if rel.ends_with(".bsl") || rel.ends_with(".xml") {
                no_links(&back.join(rel))?;
                let a = fs::read(stage.join(rel)).map_err(crate::external)?;
                let b = fs::read(back.join(rel)).map_err(crate::external)?;
                let equal = if rel.ends_with(".bsl") {
                    a == b
                } else {
                    xml_signature(&a)? == xml_signature(&b)?
                };
                if !equal {
                    return Err(Error::Invalid(format!(
                        "Обратная выгрузка отличается: {rel}"
                    )));
                }
            }
        }
        Ok(())
    })();
    let unchanged = verify_inventory(&stage, &r.inventory, &Cancellation::default());
    receipt["stagingUnchanged"] = json!(unchanged.is_ok());
    receipt["seconds"] = json!(now.elapsed().as_secs_f64());
    receipt["finishedUtc"] = json!(chrono::Utc::now().to_rfc3339());
    let platform_unchanged =
        if sha256(&fs::read(&r.executable).map_err(crate::external)?) == platform_hash {
            Ok(())
        } else {
            Err(Error::Invalid(
                "Платформа изменилась во время сборки".into(),
            ))
        };
    let result = result.and(unchanged).and(platform_unchanged);
    receipt["status"] = json!(if result.is_ok() { "passed" } else { "failed" });
    if let Err(e) = &result {
        receipt["error"] = json!(e.to_string());
    }
    if result.is_ok() {
        let bytes = fs::read(&cf).map_err(crate::external)?;
        receipt["sha256"] = json!(sha256(&bytes));
        receipt["bytes"] = json!(bytes.len());
    }
    write_json(&work.join("build-receipt.json"), &receipt)?;
    result.map(|()| NativeArtifact {
        path: cf,
        sha256: receipt["sha256"].as_str().unwrap().into(),
    })
}
pub fn normal_path_display(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(tail) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{tail}")
    } else {
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unc_display_and_wide_xml_are_linear_inputs() {
        assert_eq!(
            normal_path_display(Path::new(r"\\?\UNC\server\share\out")),
            r"\\server\share\out"
        );
        let text = format!(
            "<Root>\n{}</Root>",
            (0..10000)
                .map(|n| format!("<A>{n}</A>\n"))
                .collect::<String>()
        );
        let signature = xml_signature(text.as_bytes()).unwrap();
        assert_eq!(signature[3].as_array().unwrap().len(), 10000);
    }
    #[test]
    fn namespace_and_qname_readback() {
        let a = br#"<m:R xmlns:m="one" xmlns:v="two" v:a="v:Type"><m:A>v:T</m:A></m:R>"#;
        let b = br#"<R xmlns="one" xmlns:t="two" t:a="t:Type">
        <A>t:T</A>
        </R>"#;
        assert_eq!(xml_signature(a).unwrap(), xml_signature(b).unwrap());
        assert_ne!(
            xml_signature(b).unwrap(),
            xml_signature(br#"<R xmlns="one"><A>T</A></R>"#).unwrap()
        );
        assert_ne!(
            xml_signature(b"<R xml:space=\"preserve\"> </R>").unwrap(),
            xml_signature(b"<R xml:space=\"preserve\"></R>").unwrap()
        );
        assert_eq!(
            xml_signature(b"<R a=\"x&#13;y\">a\r\nb</R>").unwrap(),
            xml_signature(b"<R a=\"x&#13;y\">a\nb</R>").unwrap()
        );
        assert_eq!(
            xml_signature(b"<R xmlns=\"\"><A/></R>").unwrap(),
            xml_signature(b"<R><A/></R>").unwrap()
        );
        assert_eq!(
            xml_signature(b"<R xmlns=\"outer\"><A xmlns=\"\"/></R>").unwrap(),
            xml_signature(b"<p:R xmlns:p=\"outer\"><A/></p:R>").unwrap()
        );
    }
}
