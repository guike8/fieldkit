"""Developer benchmark driver. No model SDK; downloaded pages are never executed.

capture uses live MCP collection once per supplied dated seed, with pinned hashes.
score reads that frozen case through read-only tools and verifies retained bytes.
The capture phase never loads oracle.json. This is not an agent-autonomy eval.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import selectors
import subprocess

HERE = Path(__file__).resolve().parent


def read_json(path):
    return json.loads(Path(path).read_text())


def save_new(path, value):
    with Path(path).open('x') as output:
        json.dump(value, output, indent=2)
        output.write('\n')


class Client:
    def __init__(self, command, wire_log=None):
        if not isinstance(command, list) or not command or not all(isinstance(v, str) for v in command):
            raise ValueError('Supply an explicit JSON array of server command arguments')
        self.log = Path(wire_log).open('x') if wire_log else None
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.sequence = 0
        self.ready = selectors.DefaultSelector()
        self.ready.register(self.process.stdout, selectors.EVENT_READ)
        self.rpc('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'fieldkit-historical-replay', 'version': '1'}})

    def rpc(self, method, params):
        self.sequence += 1
        request = {'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}
        self.process.stdin.write(json.dumps(request) + '\n')
        self.process.stdin.flush()
        if not self.ready.select(timeout=90):
            raise TimeoutError('MCP response deadline reached; do not automatically retry collection')
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError('MCP server closed before responding')
        response = json.loads(line)
        if self.log:
            self.log.write(json.dumps({'request': request, 'response': response}) + '\n')
            self.log.flush()
        if response.get('id') != self.sequence or 'error' in response:
            raise RuntimeError(response)
        return response['result']

    def tool(self, name, arguments):
        result = self.rpc('tools/call', {'name': name, 'arguments': arguments})
        if result.get('isError'):
            raise RuntimeError(result)
        value = result['structuredContent']
        return value['data'] if value.get('command', '').startswith('case.') else value

    def close(self):
        self.ready.close()
        if self.log:
            self.log.close()
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=10)
        if self.process.returncode:
            raise RuntimeError(f'MCP server exited {self.process.returncode}')


def capture(client, task, receipt_path):
    receipt = {'benchmark': task['id'], 'track': task['track'], 'status': 'starting', 'inputs': []}
    save_new(receipt_path, receipt)
    # This file was created by this invocation; preserve progress before each network call.
    def checkpoint():
        Path(receipt_path).write_text(json.dumps(receipt, indent=2) + '\n')
    try:
        inv = client.tool('fieldkit_investigation_create', {'question': task['question'], 'mode': task['mode'], 'seeds': [i['url'] for i in task['inputs']]})
        receipt['investigation_id'] = inv['id']
        for item in task['inputs']:
            entry = {'id': item['id'], 'url': item['url'], 'expected_sha256': item['sha256'], 'status': 'planning'}
            receipt['inputs'].append(entry)
            checkpoint()
            step = client.tool('fieldkit_investigation_plan_step', {'id': inv['id'], 'job': {'command': 'http', 'target': item['url'], 'options': {}}, 'rationale': 'Inspect this explicitly supplied dated benchmark capture. Do not execute scripts, follow live links, or project observations onto other dates.'})
            entry.update({'step_id': step['id'], 'status': 'attempting'})
            checkpoint()
            report = client.tool('fieldkit_investigation_run_step', {'id': inv['id'], 'step_id': step['id']})
            entry.update({'run_id': report['run_id'], 'collector_status': report['status'], 'observed_sha256': report['data'].get('body_sha256'), 'status': 'retained'})
            checkpoint()
            if report['status'] != 'ok' or report['data'].get('http_status') != 200 or entry['observed_sha256'] != item['sha256']:
                raise ValueError(f"Blocked, incomplete or changed fixture: {item['id']}; receipt retained; no retry or silent hash update")
        receipt['status'] = 'captured'
        checkpoint()
        return receipt
    except Exception:
        receipt['status'] = 'incomplete'
        checkpoint()
        raise


def canonical(kind, raw):
    if kind == 'google_analytics_ua_property' and re.fullmatch(r'UA-[0-9]+-[0-9]+', raw, re.I):
        return raw.upper()
    if kind == 'google_adsense_publisher' and re.fullmatch(r'(?:ca-)?pub-[0-9]{16}', raw, re.I):
        return raw.lower().removeprefix('ca-')
    return None


def assess(task, oracle, reports, graph, blobs, integrity, state):
    nodes = {node['id']: node for node in graph.get('nodes', [])}
    results = []
    for item in task['inputs']:
        report = reports[item['id']]
        raw = blobs[item['id']]
        pinned = hashlib.sha256(raw).hexdigest() == item['sha256'] == report.get('data', {}).get('body_sha256')
        expected = {tuple(v) for v in oracle['expected'][item['id']]}
        extracted = report.get('data', {}).get('tracking_identifiers')
        supported = isinstance(extracted, dict) and extracted.get('response_eligible') is True
        facts, bad_spans = set(), 0
        for observation in (extracted or {}).get('identifiers', []):
            kind, value = observation.get('kind'), observation.get('value')
            start, end = observation.get('byte_start'), observation.get('byte_end')
            literal = observation.get('raw_value', '')
            valid = (type(start) is int and type(end) is int and 0 <= start < end <= len(raw)
                     and raw[start:end] == literal.encode('ascii', errors='replace')
                     and canonical(kind, literal) == value
                     and observation.get('scope') == 'contains_tracking_literal')
            if valid:
                facts.add((kind, value))
            else:
                bad_spans += 1
        graph_facts, bad_edges = set(), 0
        evidence_id = 'evidence_' + report['run_id']
        source_ids = {s['source_id'] for s in report.get('sources', []) if s.get('sha256') == item['sha256']}
        for edge in graph.get('edges', []):
            if edge.get('relation') != 'contains_tracking_literal' or evidence_id not in edge.get('evidence_ids', []):
                continue
            source, target = nodes.get(edge.get('source_id'), {}), nodes.get(edge.get('target_id'), {})
            fact = (target.get('type'), target.get('value'))
            if (source.get('type') == 'url' and source.get('value') == report['data'].get('url')
                    and edge.get('status') == 'source_observation'
                    and source_ids.intersection(edge.get('source_ids', [])) and fact in facts):
                graph_facts.add(fact)
            else:
                bad_edges += 1
        passed = (pinned and supported and report.get('status') == 'ok' and report['data'].get('http_status') == 200
                  and not extracted.get('truncated') and facts == expected and graph_facts == expected and bad_spans == 0 and bad_edges == 0)
        results.append({'id': item['id'], 'pinned_bytes': pinned, 'extraction_supported': supported,
                        'expected_facts': len(expected), 'supported_hits': len(facts & expected),
                        'unexpected_facts': len(facts - expected), 'invalid_spans': bad_spans,
                        'graph_supported_hits': len(graph_facts & expected), 'invalid_edges': bad_edges, 'passed': passed})
    pending_only = (isinstance(state.get('findings'), list) and state.get('summary') == []
                    and all(f.get('status') == 'pending_review' and f.get('source_evidence') is False
                            and f.get('origin') == 'analytical_proposal' and 'review' not in f for f in state['findings']))
    return {'benchmark': task['id'], 'core_reconstruction': 'passed' if all(r['passed'] for r in results) and integrity.get('ok') and pending_only else 'failed',
            'inputs': results, 'case_integrity_ok': integrity.get('ok') is True, 'findings_pending_only': pending_only,
            'finding_count': len(state.get('findings', [])),
            'one_seed_discovery': 'not_evaluated: five dated captures are supplied; historical reverse-index discovery is not reproduced',
            'agent_autonomy': 'not_evaluated: deterministic capture and provenance checks, not a blinded model trial',
            'interpretation': 'Human judgment still required; passing does not establish ownership, personal identity, motives or the full published network.'}


def score(client, task, oracle, receipt, case_path):
    if receipt.get('status') != 'captured' or receipt.get('benchmark') != task['id']:
        raise ValueError('A complete matching capture receipt is required')
    entries = {i['id']: i for i in receipt['inputs']}
    reports, blobs = {}, {}
    case_path = Path(case_path).resolve()
    for item in task['inputs']:
        report = client.tool('fieldkit_investigation_evidence', {'id': receipt['investigation_id'], 'step_id': entries[item['id']]['step_id']})
        sources = [s for s in report.get('sources', []) if s.get('sha256') == item['sha256']]
        if not sources or report.get('target') != item['url'] or report.get('run_id') != entries[item['id']]['run_id']:
            raise ValueError('Report does not bind the pinned input and receipt run')
        path = case_path/'blobs'/item['sha256']
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 8388608:
            raise ValueError('Expected a bounded regular retained source blob')
        reports[item['id']] = report
        blobs[item['id']] = path.read_bytes()
    graph = client.tool('fieldkit_graph_show', {})
    integrity = client.tool('fieldkit_case_verify', {})
    state = client.tool('fieldkit_investigation_show', {'id': receipt['investigation_id']})
    return assess(task, oracle, reports, graph, blobs, integrity, state)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['capture', 'score'])
    parser.add_argument('--command-json', required=True, help='JSON array launching Fieldkit MCP against the desired case')
    parser.add_argument('--receipt', required=True)
    parser.add_argument('--case-path', help='Host case directory, required for read-only source verification')
    parser.add_argument('--output', help='New result file; existing files are never overwritten')
    parser.add_argument('--wire-log', help='New JSONL protocol log; may contain untrusted source text')
    args = parser.parse_args()
    if args.action == 'score' and not args.case_path:
        parser.error('score requires --case-path')
    if args.output and Path(args.output).exists():
        parser.error('--output must name a new file')
    task = read_json(HERE/'task.json')
    client = Client(json.loads(args.command_json), args.wire_log)
    try:
        if args.action == 'capture':
            result = capture(client, task, args.receipt)
        else:
            result = score(client, task, read_json(HERE/'oracle.json'), read_json(args.receipt), args.case_path)
        if args.output:
            save_new(args.output, result)
        print(json.dumps(result, indent=2))
    finally:
        client.close()
    if args.action == 'score' and result['core_reconstruction'] != 'passed':
        raise SystemExit(1)


if __name__ == '__main__':
    main()
