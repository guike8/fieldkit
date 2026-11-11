import assert from 'node:assert/strict';
import { compilationComplete, control } from './semantic-control.mjs';

const since = Date.parse('2026-11-11T00:00:00Z');
const success = { calculated: true, running: false, last_success_at: '2026-11-11T00:00:10Z', last_failure_at: null };
assert.ok(compilationComplete(success, since));
for (const status of [
  { ...success, calculated: false }, { ...success, running: true },
  { ...success, last_success_at: null }, { ...success, last_success_at: '2026-11-10T00:00:00Z' },
  { ...success, last_failure_at: '2026-11-11T00:00:11Z' },
]) assert.ok(!compilationComplete(status, since));

const env = { APP_URL: 'http://127.0.0.1:18760', AUTO_ONTOLOGY_API_TOKEN: 'fixture-token',
  FIELDKIT_AO_CONTROL_ACTION: 'wait', FIELDKIT_AO_COMPILE_SINCE: '2026-11-11T00:00:00Z', FIELDKIT_AO_SETUP_TIMEOUT: '20' };

let tick = since;
let reads = 0;
const completed = await control({ env, clock: () => tick, sleep: async ms => { tick += ms; }, progress: () => {},
  fetchImpl: async (url, options) => {
    assert.ok(url.endsWith('/api/semantic-compilation/status'));
    assert.equal(options.headers['x-api-key'], 'fixture-token');
    reads += 1;
    return new Response(JSON.stringify(reads === 1 ? { ...success, running: true, last_success_at: null } : success));
  } });
assert.equal(reads, 2, 'A first Term is insufficient while compilation is still running');
assert.deepEqual(completed, success);

await assert.rejects(control({ env, clock: () => since, progress: () => {},
  fetchImpl: async () => new Response(JSON.stringify({ ...success, calculated: false, last_success_at: null, last_failure_at: '2026-11-11T00:00:05Z' })) }), /failed pass/);

tick = since;
await assert.rejects(control({ env: { ...env, FIELDKIT_AO_SETUP_TIMEOUT: '1' }, clock: () => tick,
  sleep: async ms => { tick += ms; }, progress: () => {},
  fetchImpl: async () => new Response(JSON.stringify({ calculated: false, running: false, last_success_at: null, last_failure_at: null })) }), /timed out/);

let frozen = false;
await control({ env: { ...env, FIELDKIT_AO_CONTROL_ACTION: 'freeze' }, progress: () => {},
  fetchImpl: async (url, options) => {
    assert.ok(url.endsWith('/api/configurations/semantic-compilation'));
    assert.equal(options.method, 'PUT');
    assert.deepEqual(JSON.parse(options.body), { enabled: false });
    frozen = true;
    return new Response('{"enabled":false}');
  } });
assert.ok(frozen, 'Freezing only disables the scheduler flag; it never calls a reset/delete route');

let calls = 0;
await control({ env: { ...env, AUTO_ONTOLOGY_API_TOKEN: '', FIELDKIT_AO_CONTROL_ACTION: 'freeze',
  AUTO_ONTOLOGY_ADMIN_EMAIL: 'fixture@fieldkit.local', AUTO_ONTOLOGY_ADMIN_PASSWORD: 'fixture-password' }, progress: () => {},
  fetchImpl: async (url, options) => {
    calls += 1;
    if (calls === 1) {
      assert.ok(url.endsWith('/api/auth/sign-in/email'));
      return new Response('{}', { headers: { 'set-cookie': 'session=fixture; HttpOnly' } });
    }
    assert.ok(url.endsWith('/api/configurations/semantic-compilation'));
    assert.equal(options.headers.Cookie, 'session=fixture');
    return new Response('{"enabled":false}');
  } });
assert.equal(calls, 2, 'Cleanup can authenticate even if initial token creation failed');
console.log('Semantic control: completion, waiting, failure, timeout and both freeze paths passed');
