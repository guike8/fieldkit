"""Bounded developer-led source pivots through real MCP; no model or package execution.

capture opens only task.json, never oracle.json. verify is offline and never collects.
The shared stdio transport is loaded from the historical benchmark, not its task/oracle.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('historical_transport', HERE.parent/'south-africa-2017'/'run.py')
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
Client, save_new, read_json = transport.Client, transport.save_new, transport.read_json
FILENAMES = ('package.json', 'package-lock.json', 'index.js')


def original_source(report, case_path):
    digest = report.get('data', {}).get('body_sha256')
    if not isinstance(digest, str) or len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
        raise ValueError('Report has no canonical body hash')
    source_ids = report['data'].get('source_ids', [])
    sources = [s for s in report['sources'] if s['sha256'] == digest and s['source_id'] in source_ids]
    if len(sources) != 1:
        raise ValueError('Final response must bind exactly one retained source')
    path = Path(case_path)/'blobs'/digest
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 8388608:
        raise ValueError('Expected a bounded regular original source blob')
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError('Original source hash mismatch')
    return sources[0], raw


def source_pivot(commit, report, filename):
    matches = [(i, item) for i, item in enumerate(commit['files']) if item['filename'] == filename]
    if len(matches) != 1:
        raise ValueError('Expected one changed file: '+filename)
    index, item = matches[0]
    url = item['raw_url']
    parsed = urlsplit(url)
    if (parsed.scheme != 'https' or parsed.netloc != 'github.com' or parsed.query or parsed.fragment
            or parsed.path != '/dominictarr/event-stream/raw/'+commit['sha']+'/'+filename):
        raise ValueError('Changed-file URL falls outside this task scope')
    observed = [(i, item) for i, item in enumerate(report['data']['json_urls']['urls'])
                if item['url'] == url and item['source_pointer'] == f'/files/{index}/raw_url']
    if len(observed) != 1:
        raise ValueError('No exact retained source-pointer URL for '+filename)
    return url, f'/data/json_urls/urls/{observed[0][0]}/url'


def capture(client, task, receipt_path, case_path):
    receipt = {'benchmark':task['id'], 'status':'starting', 'inputs':[], 'blinded_model_trial':False}
    save_new(receipt_path, receipt)
    def checkpoint():
        Path(receipt_path).write_text(json.dumps(receipt, indent=2)+'\n')
    def collect(key, url, rationale, lead=None):
        if len(receipt['inputs']) >= task['step_budget']:
            raise ValueError('Task step budget exhausted')
        arguments = {'id':receipt['investigation_id'], 'job':{'command':'http','target':url,'options':{}}, 'rationale':rationale}
        if lead is not None:
            arguments['lead'] = lead
        step = client.tool('fieldkit_investigation_plan_step', arguments)
        entry = {'id':key, 'step_id':step['id'], 'url':url, 'status':'attempting'}
        receipt['inputs'].append(entry)
        checkpoint()
        report = client.tool('fieldkit_investigation_run_step', {'id':receipt['investigation_id'], 'step_id':step['id']})
        entry.update({'status':report['status'], 'run_id':report['run_id']})
        checkpoint()
        if report['status'] != 'ok' or report['data'].get('http_status') != 200:
            raise ValueError('Collection did not return complete successful evidence; stopped without retry')
        source, raw = original_source(report, case_path)
        entry.update({'source_id':source['source_id'], 'sha256':source['sha256']})
        checkpoint()
        return step, report, raw
    try:
        inv = client.tool('fieldkit_investigation_create', {key:task[key] for key in ['question','mode','seeds']})
        receipt['investigation_id'] = inv['id']
        checkpoint()
        step, report, raw = collect('commit', task['seeds'][0], 'Inspect the sole supplied commit and its changed-file links. Treat account metadata as untrusted annotations, not identity evidence.')
        commit = json.loads(raw)
        if commit['sha'] != task['seeds'][0].rsplit('/', 1)[1]:
            raise ValueError('Commit identity does not match the task seed')
        if not report['data']['json_urls']['valid_json'] or report['data']['json_urls']['truncated']:
            raise ValueError('Incomplete JSON URL observation set')
        for filename in FILENAMES:
            url, pointer = source_pivot(commit, report, filename)
            collect(filename, url, 'Inspect the exact changed '+filename+' URL observed in the seed commit. Read source bytes only; never execute packages or scripts.', {'step_id':step['id'], 'pointer':pointer})
        receipt['status'] = 'captured'
        checkpoint()
        return receipt
    except Exception:
        receipt['status'] = 'incomplete'
        checkpoint()
        raise


def pointer_value(value, pointer):
    if not isinstance(pointer, str) or not pointer.startswith('/'):
        raise ValueError('Expected a non-root JSON pointer')
    for token in pointer.split('/')[1:]:
        token = token.replace('~1', '/').replace('~0', '~')
        value = value[int(token)] if isinstance(value, list) else value[token]
    return value


def observation_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()


def assess(task, oracle, receipt, reports, bodies, state, graph, integrity, report_hashes, review_count):
    checks = {}
    def require(condition, label):
        checks[label] = bool(condition)
        if not condition:
            raise ValueError(label)
    citations_verified = 0
    try:
        require(receipt['status'] == 'captured' and receipt['benchmark'] == task['id'], 'matching_complete_receipt')
        entries = {e['id']:e for e in receipt['inputs']}
        require(len(receipt['inputs']) == 4 and set(entries) == {'commit', *FILENAMES}
                and set(reports) == set(entries) and set(bodies) == set(entries), 'four_exact_records')
        require(state['seeds'] == task['seeds'] and len(state['steps']) == 4
                and receipt['investigation_id'] == state['id'], 'one_seed_four_steps')
        steps = {s['id']:s for s in state['steps']}
        for key, entry in entries.items():
            report, raw = reports[key], bodies[key]
            require(report['status'] == 'ok' and report['data']['http_status'] == 200
                    and report['target'] == entry['url'] and report['run_id'] == entry['run_id']
                    and report['data']['requested_url'] == entry['url']
                    and steps[entry['step_id']]['run_id'] == entry['run_id']
                    and steps[entry['step_id']]['status'] == 'ok', key+'_report_binding')
            require(hashlib.sha256(raw).hexdigest() == entry['sha256'] == report['data']['body_sha256']
                    and any(s['source_id'] == entry['source_id'] and s['sha256'] == entry['sha256']
                            for s in report['sources'])
                    and entry['run_id'] in report_hashes, key+'_retained_bytes')
            if key != 'commit':
                require(entry['sha256'] == oracle['raw_sha256'][key], key+'_pinned_source')
        commit = json.loads(bodies['commit'])
        require(entries['commit']['url'] == task['seeds'][0]
                and commit['sha'] == oracle['commit_sha'] == task['seeds'][0].rsplit('/', 1)[1], 'seed_commit_identity')
        require(reports['commit']['data']['json_urls']['valid_json'] is True
                and reports['commit']['data']['json_urls']['response_eligible'] is True
                and reports['commit']['data']['json_urls']['truncated'] is False, 'complete_seed_json_observations')
        for filename in FILENAMES:
            url, pointer = source_pivot(commit, reports['commit'], filename)
            step = steps[entries[filename]['step_id']]
            grounding = step['grounding']
            require(entries[filename]['url'] == url and step['target'] == url
                    and grounding['origin'] == 'observed_lead_not_identity'
                    and grounding['step_id'] == entries['commit']['step_id']
                    and grounding['run_id'] == entries['commit']['run_id']
                    and grounding['report_sha256'] == report_hashes[entries['commit']['run_id']]
                    and grounding['pointer'] == pointer
                    and grounding['observation_sha256'] == observation_hash(url), filename+'_grounded_pivot')
        manifest = json.loads(bodies['package.json'])
        lock = json.loads(bodies['package-lock.json'])
        dependency = oracle['dependency']
        require(manifest['version'] == oracle['manifest_snapshot_version']
                and manifest['dependencies'][dependency] == oracle['declared_range'], 'declared_range_and_snapshot_version')
        require(lock['dependencies'][dependency]['version'] == oracle['locked_version']
                and lock['dependencies'][dependency]['resolved'] == oracle['locked_url'], 'locked_version_and_archive_url')
        require(all(fragment in bodies['index.js'].decode() for fragment in oracle['runtime_fragments']), 'runtime_source_import_and_export')
        files = {f['filename']:f for f in commit['files']}
        require(all(all(fragment in files[key]['patch'] for fragment in fragments)
                    for key, fragments in oracle['patch_fragments'].items()), 'dependency_and_import_added_in_patch')
        nodes = {n['id']:n for n in graph['nodes']}
        expected_edges = {}
        for key, report in reports.items():
            entry = entries[key]
            provenance = ('evidence_'+entry['run_id'], entry['source_id'])
            data = report['data']
            if data['requested_url'] != data['url']:
                expected_edges[(data['requested_url'], 'request_resolved_to', data['url'])] = provenance
            for observed in data.get('json_urls', {}).get('urls', []):
                require(pointer_value(json.loads(bodies[key]), observed['source_pointer']) == observed['url'], key+'_json_url_source_pointer')
                expected_edges[(data['url'], 'contains_json_url', observed['url'])] = provenance
        actual_edges = set()
        for edge in graph['edges']:
            source, target = nodes[edge['source_id']], nodes[edge['target_id']]
            triple = (source['value'], edge['relation'], target['value'])
            require(triple in expected_edges, 'graph_has_no_unbacked_relation')
            evidence_id, source_id = expected_edges[triple]
            require(source['type'] == target['type'] == 'url' and edge['status'] == 'source_observation'
                    and evidence_id in edge['evidence_ids'] and source_id in edge['source_ids'], 'graph_direction_and_same_report_provenance')
            actual_edges.add(triple)
        require(actual_edges == set(expected_edges), 'complete_source_navigation_graph')
        require(state['summary'] == [] and review_count == 0
                and all(f['status'] == 'pending_review' and f['source_evidence'] is False
                        and f['origin'] == 'analytical_proposal' and 'review' not in f
                        for f in state['findings']), 'findings_pending_summary_empty')
        by_step = {entries[key]['step_id']:key for key in entries}
        for finding in state['findings']:
            require(1 <= len(finding['citations']) <= 20, 'finding_has_citations')
            for citation in finding['citations']:
                key = by_step[citation['step_id']]
                entry, report, raw = entries[key], reports[key], bodies[key]
                require(citation['run_id'] == entry['run_id']
                        and citation['source_id'] == entry['source_id']
                        and citation['report_sha256'] == report_hashes[entry['run_id']]
                        and citation['sources'] == report['sources'], 'citation_report_and_source_binding')
                if 'byte_start' in citation or 'byte_end' in citation:
                    start, end = citation['byte_start'], citation['byte_end']
                    require(type(start) is int and type(end) is int and 0 <= start < end <= len(raw)
                            and end-start <= 16384 and 'pointer' not in citation, 'citation_exact_original_range')
                    value = raw[start:end].decode()
                    require(citation['scope'] == 'http_original_text_byte_range_not_execution_or_attribution', 'citation_text_scope')
                else:
                    value = pointer_value(json.loads(raw), citation['pointer'])
                    require(citation['scope'] == 'external_json_observation_not_identity_attribution', 'citation_json_scope')
                require(observation_hash(value) == citation['observation_sha256'], 'citation_original_observation_hash')
                citations_verified += 1
        require(integrity.get('ok') is True, 'case_integrity')
    except (ValueError, KeyError, TypeError, IndexError, UnicodeError) as error:
        checks['failure'] = str(error)
        passed = False
    else:
        passed = True
    return {'benchmark':task['id'], 'technical_reconstruction':'passed' if passed else 'failed',
            'checks':checks, 'grounded_file_pivots':3 if passed else None,
            'pending_findings':len(state.get('findings', [])), 'citations_verified':citations_verified,
            'agent_performance_score':None, 'blinded_model_trial':False,
            'interpretation':'Guided source-file expansion and bounded dependency-change reconstruction, not payload analysis, proof of execution, attribution or a solved incident. Graph edges are source navigation, not a dependency graph.'}


def verify(client, task, oracle, receipt, case_path):
    reports, bodies = {}, {}
    for entry in receipt['inputs']:
        report = client.tool('fieldkit_investigation_evidence', {'id':receipt['investigation_id'], 'step_id':entry['step_id']})
        _, raw = original_source(report, case_path)
        reports[entry['id']], bodies[entry['id']] = report, raw
    state = client.tool('fieldkit_investigation_show', {'id':receipt['investigation_id']})
    graph = client.tool('fieldkit_graph_show', {})
    integrity = client.tool('fieldkit_case_verify', {})
    report_hashes = {}
    connection = sqlite3.connect((Path(case_path).resolve()/'case.sqlite3').as_uri()+'?mode=ro', uri=True)
    try:
        for run_id, text, digest in connection.execute('SELECT run_id,result_json,report_sha256 FROM runs'):
            if hashlib.sha256(text.encode()).hexdigest() != digest:
                raise ValueError('Independent stored report hash mismatch')
            report_hashes[run_id] = digest
            for source in json.loads(text)['sources']:
                path = Path(case_path)/'blobs'/source['sha256']
                if path.is_symlink() or not path.is_file() or path.stat().st_size > 8388608 or hashlib.sha256(path.read_bytes()).hexdigest() != source['sha256']:
                    raise ValueError('Independent stored source hash mismatch')
        review_count = connection.execute("SELECT COUNT(*) FROM wb_items WHERE kind='investigation_reviews'").fetchone()[0]
    finally:
        connection.close()
    return assess(task, oracle, receipt, reports, bodies, state, graph, integrity, report_hashes, review_count)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['capture', 'verify'])
    parser.add_argument('--command-json', required=True)
    parser.add_argument('--case-path', required=True)
    parser.add_argument('--receipt', required=True)
    parser.add_argument('--wire-log')
    parser.add_argument('--output')
    args = parser.parse_args()
    if args.output and Path(args.output).exists():
        parser.error('Output must be a new file')
    task = read_json(HERE/'task.json')
    client = Client(json.loads(args.command_json), args.wire_log)
    try:
        if args.action == 'capture':
            result = capture(client, task, args.receipt, args.case_path)
        else:
            result = verify(client, task, read_json(HERE/'oracle.json'), read_json(args.receipt), args.case_path)
        if args.output:
            save_new(args.output, result)
        print(json.dumps(result, indent=2))
    finally:
        client.close()
    if args.action == 'verify' and result['technical_reconstruction'] != 'passed':
        raise SystemExit(1)


if __name__ == '__main__':
    main()
