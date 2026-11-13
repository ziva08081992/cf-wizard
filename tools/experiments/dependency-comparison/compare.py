"""Compare unchanged Python/Rust services on the same real export; no native CF build."""
from __future__ import annotations
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import runpy
import statistics
import subprocess
import sys
from time import perf_counter
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "tools/1c-delivery"))
from cf_wizard import adapters
from cf_wizard.dependency_index import IndexReader, descriptor_inventory, update_index
from cf_wizard.helper_worker import HELPER_SHA256

CASES = {
    "module": ("CommonModule.ZipАрхивы",),
    "catalog": ("Catalog.Валюты",),
    "register": ("AccumulationRegister.АмортизацияОС",),
    "mixed": ("AccumulationRegister.АмортизацияОС", "Catalog.Валюты", "CommonModule.ZipАрхивы"),
}
def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()
def save(path: Path, value) -> None:
    path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
def canonical(value) -> str:
    return hashlib.sha256(json.dumps(value,ensure_ascii=True,sort_keys=True,separators=(",",":")).encode()).hexdigest()
def run_python(job: dict, helper: Path) -> None:
    started=perf_counter()
    reader=adapters.DesignerExport(helper)
    source=reader.read(Path(job["source"]))
    init_seconds=perf_counter()-started
    selected=tuple(sorted(job["selected"]))
    index=Path(job["index"]) if job.get("index") else None
    package=adapters.TechnicalPackage(reader,Path(job["logs"]))
    original=adapters.resolve_dependencies
    analysis_seconds=[]
    snapshot_seconds=[]
    inventory_seconds=[]
    original_markers=adapters._source_markers
    original_inventory=adapters.descriptor_inventory
    def markers(*args,**kwargs):
        begin=perf_counter();value=original_markers(*args,**kwargs)
        snapshot_seconds.append(perf_counter()-begin);return value
    def inventory(*args,**kwargs):
        begin=perf_counter();value=original_inventory(*args,**kwargs)
        inventory_seconds.append(perf_counter()-begin);return value
    def timed(*args,**kwargs):
        begin=perf_counter()
        result=original(*args,**kwargs)
        analysis_seconds.append(perf_counter()-begin)
        return result
    total=perf_counter()
    with patch.object(adapters,"resolve_dependencies",timed),patch.object(adapters,"_source_markers",markers),patch.object(adapters,"descriptor_inventory",inventory):
        preview=package.prepare(source,None,selected,None,Path(job["logs"]),index)
    preview_seconds=perf_counter()-total
    plan=package._previews[preview.log]
    result=plan.result
    if index and not result["index_message"].startswith("Индекс SQLite: описаний "):
        raise RuntimeError("Python did not use the shared SQLite index: "+result["index_message"])
    core=plan.writer.gi_frame.f_locals["res"]
    stubs={}
    for row in result["stubs"]:
        key=row["object"]
        stubs[key]={"primary":row["referred_by"],"causes":result["causes"][key]}
    normalized={
        "selected":sorted(result["selected"]),"stubs":stubs,
        "members":{key:sorted(tag+"."+name for tag,name in stub.members) for key,stub in core.stubs.items()},
        "warnings":sorted(result["predefined_code_mismatch"],key=lambda v:v["object"]),
    }
    output={"engine":"python","analysis_seconds":sum(analysis_seconds),"preview_seconds":preview_seconds,
        "initialization_seconds":init_seconds,"index_opened":index is not None,
        "snapshot_seconds":sum(snapshot_seconds),"analysis_excluding_snapshot_seconds":sum(analysis_seconds),
        "descriptor_inventory_seconds":sum(inventory_seconds),
        "index_message":result["index_message"],"source_identity":source.identity,
        "normalized":normalized,"read_hashes":plan.hashes.copy(),"stub_count":len(stubs),
        "selected_count":len(selected),"journal":str(preview.log)}
    save(Path(job["output"]),output)
    package.discard(preview)
def child(job_file: Path,helper: Path) -> None:
    job=json.loads(job_file.read_text("utf-8-sig"))
    try: run_python(job,helper)
    except Exception as error:
        save(Path(job["output"]),{"engine":"python","error":type(error).__name__+": "+str(error)})
        raise
def summary(rows:list[dict])->list[dict]:
    output=[]
    for case in CASES:
        for mode in ("xml","sqlite"):
            group=[r for r in rows if r["case"]==case and r["mode"]==mode and not r["warmup"]]
            result={"case":case,"mode":mode}
            for engine in ("python","rust"):
                values=[r for r in group if r["engine"]==engine]
                if not values:continue
                result[engine]={"samples":len(values),"stubs":values[0]["stub_count"]}
                for metric in ("analysis_seconds","analysis_excluding_snapshot_seconds","preview_seconds","snapshot_seconds"):
                    times=[r[metric] for r in values]
                    result[engine][metric]={"median":statistics.median(times),"min":min(times),"max":max(times)}
            if "python" in result and "rust" in result:
                result["analysis_speedup"]=result["python"]["analysis_excluding_snapshot_seconds"]["median"]/result["rust"]["analysis_excluding_snapshot_seconds"]["median"]
                result["unadjusted_resolver_call_ratio"]=result["python"]["analysis_seconds"]["median"]/result["rust"]["analysis_seconds"]["median"]
                result["preview_speedup"]=result["python"]["preview_seconds"]["median"]/result["rust"]["preview_seconds"]["median"]
            output.append(result)
    return output
def main():
    p=argparse.ArgumentParser()
    p.add_argument("--source",type=Path,required=True);p.add_argument("--helper",type=Path,required=True)
    p.add_argument("--out",type=Path,required=True);p.add_argument("--rust-exe",type=Path,required=True)
    p.add_argument("--samples",type=int,default=3);p.add_argument("--cases",nargs="+",choices=CASES,default=list(CASES))
    p.add_argument("--modes",nargs="+",choices=("xml","sqlite"),default=["xml","sqlite"])
    p.add_argument("--child",type=Path);p.add_argument("--resume",action="store_true")
    a=p.parse_args()
    if a.child:child(a.child,a.helper);return
    a.out.mkdir(parents=True,exist_ok=True)
    jobs=a.out/"jobs";jobs.mkdir(exist_ok=True)
    reader=adapters.DesignerExport(a.helper);source,descriptors=reader.describe(a.source)
    before=descriptor_inventory(source.root,descriptors)
    markers={n:digest(source.root/n) for n in ("Configuration.xml","ConfigDumpInfo.xml") if (source.root/n).is_file()}
    for case in a.cases:
        assert set(CASES[case])<=set(source.objects),case
    manifest={"configuration":source.configuration,"source":str(source.root),"identity":source.identity,
        "objects":len(source.objects),"descriptors":len(descriptors),"markers":markers,
        "python":sys.version,"platform":platform.platform(),"helper_sha256":digest(a.helper),
        "rust_exe_sha256":digest(a.rust_exe),"samples":a.samples,"cases":{k:CASES[k] for k in a.cases},
        "method":"Fresh headless child per sample; catalog initialization excluded; preview includes production guards/logging; adjusted analysis excludes the two export-marker snapshot calls inside Rust (Python performs them outside its resolver); analysis still includes file IO, per-file guards and index open, not pure CPU; one warmup each; alternating Python/Rust order; OS caches not cleared; no staging/CF/native/Git HTTP."}
    save(a.out/"manifest.json",manifest)
    index=a.out/"shared-legacy.sqlite3"
    if "sqlite" in a.modes:
        print("Preparing one shared SQLite index…",flush=True)
        begin=perf_counter();namespace=runpy.run_path(str(a.helper))
        report=update_index(index,source.root,source.identity,descriptors,namespace,HELPER_SHA256,lambda:None)
        namespace["_children_cache"].clear()
        save(a.out/"index.json",{"seconds":perf_counter()-begin,"bytes":index.stat().st_size,"counts":dict(report),"sha256":digest(index)})
        print("Index ready:",index.stat().st_size,"bytes",flush=True)
    index_before=digest(index) if index.exists() else None
    rows=[];expected={};source_reads={}
    if a.resume and (a.out/"runs.json").exists():
        rows=json.loads((a.out/"runs.json").read_text("utf-8"))
    for case in a.cases:
        for mode in a.modes:
            for iteration in range(-1,a.samples):
                for engine in (("python","rust") if iteration%2 else ("rust","python")):
                    label=f"{case}-{mode}-{'warmup' if iteration<0 else iteration}-{engine}"
                    result_file=jobs/(label+".result.json")
                    job={"source":str(source.root),"selected":CASES[case],"index":str(index) if mode=="sqlite" else None,"logs":str(a.out/"logs"),"output":str(result_file)}
                    job_file=jobs/(label+".json");save(job_file,job)
                    if not (a.resume and result_file.exists()):
                        command=([sys.executable,"-B","-X","utf8",str(Path(__file__).resolve()),"--source",str(source.root),"--helper",str(a.helper),"--out",str(a.out),"--rust-exe",str(a.rust_exe),"--child",str(job_file)]
                            if engine=="python" else [str(a.rust_exe),str(job_file)])
                        call=perf_counter()
                        done=subprocess.run(command,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=600,shell=False)
                        (jobs/(label+".console.txt")).write_bytes(done.stdout)
                        if done.returncode:raise RuntimeError(f"{label} failed: {done.stdout.decode('utf-8','replace')[-1800:]}")
                        outer=perf_counter()-call
                    else:outer=None
                    row=json.loads(result_file.read_text("utf-8-sig"))
                    if row.get("error"):raise RuntimeError(row["error"])
                    row.update(case=case,mode=mode,iteration=iteration,warmup=iteration<0,process_seconds=outer)
                    norm=row["normalized"];key=case
                    if key not in expected:expected[key]=norm
                    if norm!=expected[key]:
                        save(a.out/"mismatch.json",{"label":label,"expected":expected[key],"actual":norm})
                        raise RuntimeError(f"Composition mismatch: {label}; see mismatch.json")
                    for path,sha in row["read_hashes"].items():
                        if path in source_reads and source_reads[path]!=sha:raise RuntimeError("Source changed: "+path)
                        source_reads[path]=sha
                    row["composition_sha256"]=canonical(norm)
                    compact={k:v for k,v in row.items() if k not in ("normalized","read_hashes")}
                    rows=[r for r in rows if not(r["case"]==case and r["mode"]==mode and r["iteration"]==iteration and r["engine"]==engine)]
                    rows.append(compact);save(a.out/"runs.json",rows)
                    save(a.out/"summary.json",summary(rows))
                    print(f"{label}: analysis={row['analysis_seconds']:.4f}s preview={row['preview_seconds']:.4f}s stubs={row['stub_count']}",flush=True)
    if descriptor_inventory(source.root,descriptors)!=before:raise RuntimeError("Descriptor inventory changed during benchmark")
    if any(digest(source.root/n)!=sha for n,sha in markers.items()):raise RuntimeError("Export markers changed during benchmark")
    if index_before and digest(index)!=index_before:raise RuntimeError("Shared SQLite changed during read-only runs")
    save(a.out/"consistency.json",{"passed":True,"matching_cases":len(expected),"common_result_across_engines_and_modes":True,"stable_tracked_source_files":len(source_reads),"source_markers":markers,"index_unchanged":True})
    print("DONE: equal results and stable source/index.",flush=True)
if __name__=="__main__":main()
