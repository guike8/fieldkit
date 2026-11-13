"""Offline scorer regressions; these synthetic bytes are not historical evidence."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('benchmark_driver', Path(__file__).with_name('run.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class ScoringTests(unittest.TestCase):
    def setUp(self):
        self.task = {'id': 'synthetic', 'inputs': []}
        self.oracle = {'expected': {}}
        self.reports, self.blobs = {}, {}
        self.graph = {'nodes': [], 'edges': []}
        for key, literal in [('earlier', 'UA-101199457-4'), ('later', None)]:
            body = (f"<script>'{literal}'</script>" if literal else '<html>no literal</html>').encode()
            digest = hashlib.sha256(body).hexdigest()
            self.task['inputs'].append({'id': key, 'sha256': digest})
            self.blobs[key] = body
            identifiers = []
            expected = []
            if literal:
                expected = [['google_analytics_ua_property', literal]]
                start = body.index(literal.encode())
                identifiers = [{'kind': expected[0][0], 'value': literal, 'raw_value': literal, 'byte_start': start, 'byte_end': start + len(literal), 'scope': 'contains_tracking_literal'}]
                self.graph['nodes'] += [{'id': 'url', 'type': 'url', 'value': 'https://example.invalid/earlier'}, {'id': 'property', 'type': expected[0][0], 'value': literal}]
                self.graph['edges'].append({'source_id': 'url', 'target_id': 'property', 'relation': 'contains_tracking_literal', 'status': 'source_observation', 'evidence_ids': ['evidence_run_earlier'], 'source_ids': ['src_earlier']})
            self.oracle['expected'][key] = expected
            self.reports[key] = {'run_id': 'run_' + key, 'status': 'ok', 'sources': [{'source_id': 'src_' + key, 'sha256': digest}], 'data': {'url': 'https://example.invalid/' + key, 'body_sha256': digest, 'http_status': 200, 'tracking_identifiers': {'identifiers': identifiers, 'response_eligible': True, 'truncated': False}}}
        self.state = {'summary': [], 'findings': []}
        self.integrity = {'ok': True}

    def score(self):
        return driver.assess(self.task, self.oracle, self.reports, self.graph, self.blobs, self.integrity, self.state)

    def assertFailed(self):
        self.assertEqual(self.score()['core_reconstruction'], 'failed')

    def test_complete_synthetic_evidence_and_negative_control(self):
        result = self.score()
        self.assertEqual(result['core_reconstruction'], 'passed')
        self.assertEqual(result['inputs'][0]['supported_hits'], 1)
        self.assertIn('not_evaluated', result['agent_autonomy'])

    def test_absent_capability_does_not_pass_negative_control(self):
        del self.reports['later']['data']['tracking_identifiers']
        self.assertFalse(self.score()['inputs'][1]['passed'])
        self.assertFailed()

    def test_source_mutation_blocks_credit(self):
        self.blobs['earlier'] += b'changed'
        self.assertFalse(self.score()['inputs'][0]['pinned_bytes'])
        self.assertFailed()

    def test_original_byte_range_required(self):
        self.reports['earlier']['data']['tracking_identifiers']['identifiers'][0]['byte_start'] += 1
        self.assertEqual(self.score()['inputs'][0]['invalid_spans'], 1)
        self.assertFailed()

    def test_distinct_properties_not_collapsed_to_account(self):
        self.oracle['expected']['earlier'][0][1] = 'UA-101199457-2'
        self.assertEqual(self.score()['inputs'][0]['unexpected_facts'], 1)
        self.assertFailed()

    def test_temporal_carry_forward_fails(self):
        observation = copy.deepcopy(self.reports['earlier']['data']['tracking_identifiers']['identifiers'][0])
        self.reports['later']['data']['tracking_identifiers']['identifiers'].append(observation)
        self.assertFailed()
        edge = copy.deepcopy(self.graph['edges'][0])
        edge['evidence_ids'] = ['evidence_run_later']
        self.graph['edges'].append(edge)
        self.assertGreater(self.score()['inputs'][1]['invalid_edges'], 0)

    def test_graph_requires_same_report_source_and_direction(self):
        edge = self.graph['edges'][0]
        edge['source_ids'] = ['foreign_source']
        self.assertFailed()
        edge['source_ids'] = ['src_earlier']
        edge['source_id'], edge['target_id'] = edge['target_id'], edge['source_id']
        self.assertFailed()

    def test_missing_graph_or_truncated_extraction_fails(self):
        self.graph['edges'] = []
        self.assertFailed()
        self.setUp()
        self.reports['earlier']['data']['tracking_identifiers']['truncated'] = True
        self.assertFailed()

    def test_approval_or_integrity_failure_blocks_result(self):
        self.integrity['ok'] = False
        self.assertFailed()
        self.integrity['ok'] = True
        self.state['findings'] = [{'status': 'accepted', 'source_evidence': False, 'review': {'decision': 'accepted'}}]
        self.assertFailed()

    def test_publisher_normalization_preserves_full_property(self):
        self.assertEqual(driver.canonical('google_adsense_publisher', 'Ca-PuB-8264869885899896'), 'pub-8264869885899896')
        self.assertEqual(driver.canonical('google_analytics_ua_property', 'ua-101199457-4'), 'UA-101199457-4')
        self.assertIsNone(driver.canonical('google_analytics_ua_property', 'UA-101199457'))


class DiscoveryMetadataTests(unittest.TestCase):
    """Input/outcome regression checks, not evidence of agent performance."""

    def test_seed_only_task_does_not_supply_the_answer_or_replay_captures(self):
        task = driver.read_json(Path(__file__).with_name('discovery-task.json'))
        self.assertEqual(task['seeds'], ['http://www.publicopinion.co.za:80/'])
        self.assertEqual(task['step_budget'], 10)
        self.assertNotIn('inputs', task)
        self.assertNotIn('expected', task)
        encoded = driver.json.dumps(task)
        for forbidden in ['web.archive.org/web/', 'pub-8264869885899896', 'UA-101199457', 'wmc-scams.com']:
            self.assertNotIn(forbidden, encoded)

    def test_blocked_measurement_never_claims_discovery_or_agent_credit(self):
        run = driver.read_json(Path(__file__).with_name('discovery-run.json'))
        self.assertEqual(run['status'], 'discovery_blocked')
        self.assertIs(run['discovery_pass'], False)
        self.assertIsNone(run['agent_performance_score'])
        self.assertEqual(run['source_http_status'], 429)
        self.assertEqual(run['outbound_collector_requests'], 1)
        for field in ['additional_websites_verified', 'graph_edges', 'findings', 'human_reviews', 'paid_api_calls']:
            self.assertEqual(run[field], 0)
        self.assertFalse(run['historical_index_query_performed'])
        feasibility = driver.read_json(Path(__file__).with_name('discovery-feasibility.json'))
        self.assertTrue(all(not s['historical_data_retrieved'] for s in feasibility['sources']))


if __name__ == '__main__':
    unittest.main()
