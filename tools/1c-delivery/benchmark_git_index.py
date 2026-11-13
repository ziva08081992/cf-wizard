"""Reproducible synthetic timings; never connects to a real repository or infobase."""
import argparse
from collections import Counter
from hashlib import sha256
import json
from pathlib import Path
from time import perf_counter
from unittest.mock import patch

from test_cf_wizard_git_index_reuse import GitIndexReuseTests


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--objects", type=int, default=1000)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    case = GitIndexReuseTests()
    case.setUp()
    reads = Counter()
    phase = ["open"]
    original_read = Path.read_bytes

    def counted_read(path):
        if path.is_relative_to(case.factory.cache):
            if path.suffix == '.xml':
                reads['xml_' + phase[0]] += 1
            elif path.parent.name == 'blobs':
                reads['blob_cache_' + phase[0]] += 1
        return original_read(path)

    reading = patch.object(Path, 'read_bytes', counted_read)
    reading.start()
    try:
        bodies = dict(case.bodies)
        names = "".join(f"<Catalog>Perf{i:05}</Catalog>" for i in range(args.objects))
        bodies["Configuration.xml"] = bodies["Configuration.xml"].replace(b"</ChildObjects>", names.encode() + b"</ChildObjects>")
        for i in range(args.objects):
            bodies[f"Catalogs/Perf{i:05}.xml"] = (f"<MetaDataObject><Catalog><Properties><Name>Perf{i:05}</Name>"
                                                "</Properties><ChildObjects/></Catalog></MetaDataObject>").encode()
        case.client.add(bodies)
        results = []
        index = None
        previous_bytes = None
        for scenario in ("cold", "same_sha", "bsl_only", "one_xml"):
            if scenario == "bsl_only":
                bodies["Documents/Заказ/Ext/ObjectModule.bsl"] = b"// changed module"
                case.client.add(bodies)
            if scenario == "one_xml":
                bodies["Catalogs/Perf00000.xml"] += b"<!-- changed -->"
                case.client.add(bodies)
            before = case.client.requests.copy()
            reads_before = reads.copy()
            start = perf_counter()
            phase[0] = 'open'
            source = case.open()
            opened = perf_counter()
            phase[0] = 'check'
            ready = index is not None and case.ready(source, index)
            checked = perf_counter()
            phase[0] = 'update'
            update = None
            if not ready:
                update = case.wizard.refresh_index(source, "")
                index = update.path
            finished = perf_counter()
            current = sha256(index.read_bytes()).hexdigest()
            if ready and current != previous_bytes:
                raise AssertionError("Readiness changed SQLite")
            row = dict(scenario=scenario, inputs=len(source.git.index_inputs.entries),
                       open_ms=round((opened-start)*1000, 3), check_ms=round((checked-opened)*1000, 3),
                       update_ms=round((finished-checked)*1000, 3), ready=ready,
                       requests=dict(case.client.requests-before), sqlite_bytes=index.stat().st_size,
                       path_read_bytes_calls=dict(reads-reads_before),
                       counts=(dict(added=update.added, changed=update.changed, removed=update.removed, unchanged=update.unchanged)
                               if update else None))
            if update:
                row['journal'] = update.log.read_text(encoding='utf8').splitlines()[-2:]
            results.append(row)
            previous_bytes = current
            print(json.dumps(row, ensure_ascii=True), flush=True)
        args.output.write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf8")
    finally:
        reading.stop()
        case.doCleanups()


if __name__ == "__main__":
    main()
