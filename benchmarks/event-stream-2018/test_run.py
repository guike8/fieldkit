"""Synthetic offline verifier tests. These bytes are NOT incident evidence."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('driver', Path(__file__).with_name('run.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class VerifierTests(unittest.TestCase):
    def setUp(self):
        sha = 'a'*40
        seed = 'https://api.github.com/repos/dominictarr/event-stream/commits/'+sha
        self.task = {'id':'synthetic', 'seeds':[seed]}
        self.oracle = {'commit_sha':sha, 'dependency':'sample-dependency', 'declared_range':'^0.1.0',
                       'locked_version':'0.1.0', 'locked_url':'https://registry.example/sample.tgz',
                       'manifest_snapshot_version':'1.0.0',
                       'runtime_fragments':["require('sample-dependency')", 'exports.example = example'],
                       'patch_fragments':{'package.json':['+added dependency'], 'index.js':['+added import']},
                       'raw_sha256':{}}
        commit = {'sha':sha, 'files':[]}
        for filename in driver.FILENAMES:
            commit['files'].append({'filename':filename,
                'raw_url':'https://github.com/dominictarr/event-stream/raw/'+sha+'/'+filename,
                'patch':'+added dependency' if filename == 'package.json' else '+added import'})
        manifest = {'version':'1.0.0','dependencies':{'sample-dependency':'^0.1.0'}}
        lock = {'dependencies':{'sample-dependency':{'version':'0.1.0','resolved':'https://registry.example/sample.tgz'}}}
        self.bodies = {'commit':json.dumps(commit).encode(), 'package.json':json.dumps(manifest).encode(),
                       'package-lock.json':json.dumps(lock).encode(),
                       'index.js':b"const example = require('sample-dependency');\nexports.example = example;\n"}
        self.receipt = {'benchmark':'synthetic','status':'captured','investigation_id':'inv','inputs':[]}
        self.reports, self.hashes = {}, {}
        for index, key in enumerate(['commit', *driver.FILENAMES]):
            url = seed if key == 'commit' else commit['files'][index-1]['raw_url']
            final = url if key == 'commit' else url.replace('https://github.com/', 'https://raw.githubusercontent.com/').replace('/raw/', '/')
            digest = hashlib.sha256(self.bodies[key]).hexdigest()
            entry = {'id':key,'step_id':'step'+str(index),'run_id':'run'+str(index),
                     'url':url,'source_id':'src'+str(index),'sha256':digest,'status':'ok'}
            self.receipt['inputs'].append(entry)
            urls = [{'url':f['raw_url'],'source_pointer':f'/files/{i}/raw_url'} for i,f in enumerate(commit['files'])] if key == 'commit' else []
            self.reports[key] = {'run_id':entry['run_id'],'command':'http','target':url,'status':'ok',
                'sources':[{'source_id':entry['source_id'],'sha256':digest,'kind':'http','http_status':200,
                            'media_type':'application/json' if key == 'commit' else 'text/plain'}],
                'data':{'requested_url':url,'url':final,'http_status':200,'body_sha256':digest,
                        'json_urls':{'urls':urls,'valid_json':key=='commit','response_eligible':key=='commit','truncated':False}}}
            self.hashes[entry['run_id']] = driver.observation_hash(self.reports[key])
            if key != 'commit':
                self.oracle['raw_sha256'][key] = digest
        self.state = {'id':'inv','seeds':[seed],'steps':[],'findings':[],'summary':[]}
        self.graph = {'nodes':[],'edges':[]}
        node_ids = {}
        def node(url):
            if url not in node_ids:
                node_ids[url] = 'node'+str(len(node_ids))
                self.graph['nodes'].append({'id':node_ids[url],'type':'url','value':url})
            return node_ids[url]
        def edge(source, relation, target, entry):
            self.graph['edges'].append({'source_id':node(source),'target_id':node(target),'relation':relation,
                'status':'source_observation','evidence_ids':['evidence_'+entry['run_id']],'source_ids':[entry['source_id']]})
        for entry in self.receipt['inputs']:
            key = entry['id']
            step = {'id':entry['step_id'],'run_id':entry['run_id'],'target':entry['url'],'status':'ok'}
            if key != 'commit':
                _, pointer = driver.source_pivot(commit, self.reports['commit'], key)
                step['grounding'] = {'origin':'observed_lead_not_identity','step_id':'step0','run_id':'run0',
                    'report_sha256':self.hashes['run0'],'pointer':pointer,'observation_sha256':driver.observation_hash(entry['url'])}
                edge(seed,'contains_json_url',entry['url'],self.receipt['inputs'][0])
                edge(entry['url'],'request_resolved_to',self.reports[key]['data']['url'],entry)
                raw = self.bodies[key]
                citation = {'step_id':entry['step_id'],'run_id':entry['run_id'],'source_id':entry['source_id'],
                    'report_sha256':self.hashes[entry['run_id']],'sources':self.reports[key]['sources'],
                    'byte_start':0,'byte_end':len(raw),'observation_sha256':driver.observation_hash(raw.decode()),
                    'scope':'http_original_text_byte_range_not_execution_or_attribution'}
                self.state['findings'].append({'status':'pending_review','source_evidence':False,
                    'origin':'analytical_proposal','citations':[citation]})
            self.state['steps'].append(step)
        self.integrity = {'ok':True}
        self.review_count = 0

    def score(self):
        return driver.assess(self.task,self.oracle,self.receipt,self.reports,self.bodies,
                             self.state,self.graph,self.integrity,self.hashes,self.review_count)

    def assertFailed(self):
        self.assertEqual(self.score()['technical_reconstruction'],'failed')

    def replace_body(self, key, raw):
        self.bodies[key] = raw
        digest = hashlib.sha256(raw).hexdigest()
        entry = next(e for e in self.receipt['inputs'] if e['id'] == key)
        entry['sha256'] = digest
        self.reports[key]['sources'][0]['sha256'] = digest
        self.reports[key]['data']['body_sha256'] = digest
        self.oracle['raw_sha256'][key] = digest

    def test_complete_synthetic_source_pivots_citations_and_graph(self):
        result = self.score()
        self.assertEqual(result['technical_reconstruction'],'passed')
        self.assertEqual(result['grounded_file_pivots'],3)
        self.assertEqual(result['citations_verified'],3)
        self.assertIsNone(result['agent_performance_score'])

    def test_source_mutation_fails(self):
        self.bodies['index.js'] += b'changed'
        self.assertFailed()

    def test_missing_extraction_cannot_pass(self):
        self.reports['commit']['data']['json_urls']['valid_json'] = False
        self.assertFailed()

    def test_wrong_source_pointer_cannot_ground_a_pivot(self):
        self.reports['commit']['data']['json_urls']['urls'][0]['source_pointer'] = '/unrelated'
        self.assertFailed()

    def test_unobserved_or_foreign_target_fails(self):
        self.receipt['inputs'][1]['url'] = 'https://other.example/file'
        self.assertFailed()

    def test_grounding_binds_exact_parent_report(self):
        self.state['steps'][1]['grounding']['report_sha256'] = 'wrong'
        self.assertFailed()

    def test_missing_request_response_binding_fails(self):
        self.graph['edges'] = [e for e in self.graph['edges'] if e['relation'] != 'request_resolved_to']
        self.assertFailed()

    def test_wrong_graph_direction_and_report_provenance_fail(self):
        saved = copy.deepcopy(self.graph)
        edge = self.graph['edges'][0]
        edge['source_id'], edge['target_id'] = edge['target_id'], edge['source_id']
        self.assertFailed()
        self.graph = saved
        self.graph['edges'][0]['source_ids'] = ['foreign_source']
        self.assertFailed()

    def test_later_payload_version_cannot_replace_the_observed_lock(self):
        raw = self.bodies['package-lock.json'].replace(b'"version": "0.1.0"',b'"version": "0.1.1"')
        self.replace_body('package-lock.json',raw)
        self.assertFailed()
        self.assertFalse(self.score()['checks']['locked_version_and_archive_url'])

    def test_import_text_is_required_not_just_a_manifest(self):
        self.replace_body('index.js',b'// no import\n')
        self.assertFailed()
        self.assertFalse(self.score()['checks']['runtime_source_import_and_export'])

    def test_citation_must_recover_original_bytes_and_scope(self):
        citation = self.state['findings'][0]['citations'][0]
        citation['byte_end'] -= 1
        self.assertFailed()
        citation['byte_end'] += 1
        citation['scope'] = 'http_response_metadata_not_page_content'
        self.assertFailed()

    def test_reviews_accepted_summary_or_integrity_failure_fail(self):
        self.review_count = 1
        self.assertFailed()
        self.review_count = 0
        self.state['summary'] = [{'content':'accepted'}]
        self.assertFailed()
        self.state['summary'] = []
        self.integrity['ok'] = False
        self.assertFailed()

    def test_capture_does_not_open_an_oracle_or_load_any_other_json_file(self):
        fixture = self
        class FakeClient:
            def __init__(self):
                self.index = 0
            def tool(self, name, arguments):
                if name == 'fieldkit_investigation_create':
                    return {'id':'inv'}
                if name == 'fieldkit_investigation_plan_step':
                    return {'id':'step'+str(self.index)}
                if name == 'fieldkit_investigation_run_step':
                    key = ['commit', *driver.FILENAMES][self.index]
                    self.index += 1
                    return fixture.reports[key]
                raise AssertionError('Unexpected tool: '+name)
        task = dict(self.task, question='Synthetic source question', mode='infrastructure', step_budget=6)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'blobs').mkdir()
            for key, report in self.reports.items():
                digest = report['data']['body_sha256']
                report['data']['source_ids'] = [report['sources'][0]['source_id']]
                (root/'blobs'/digest).write_bytes(self.bodies[key])
            with patch.object(driver, 'read_json', side_effect=AssertionError('Capture must not read oracle files')):
                receipt = driver.capture(FakeClient(), task, root/'receipt.json', root)
            self.assertEqual(receipt['status'], 'captured')
            self.assertEqual(len(receipt['inputs']), 4)

    def test_real_task_supplies_one_commit_not_the_dependency_answer(self):
        task = driver.read_json(Path(__file__).with_name('task.json'))
        self.assertEqual(len(task['seeds']),1)
        self.assertEqual(task['step_budget'],6)
        text = json.dumps(task)
        for answer in ['flatmap-stream','^0.1.0','0.1.1','3.3.5']:
            self.assertNotIn(answer,text)
        self.assertNotIn('inputs',task)
        self.assertNotIn('expected',task)


if __name__ == '__main__':
    unittest.main()
