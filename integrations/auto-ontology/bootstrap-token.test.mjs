import assert from 'node:assert/strict';

async function scenario(failCompilation, tokenOnly = false, enableOnly = false) {
  const saved = { fetch: globalThis.fetch, write: process.stdout.write,
    origin: process.env.APP_URL, email: process.env.AUTO_ONTOLOGY_ADMIN_EMAIL,
    password: process.env.AUTO_ONTOLOGY_ADMIN_PASSWORD,
    tokenOnly: process.env.FIELDKIT_AO_TOKEN_ONLY, enableOnly: process.env.FIELDKIT_AO_ENABLE_ONLY,
    token: process.env.AUTO_ONTOLOGY_API_TOKEN };
  process.env.APP_URL = 'http://127.0.0.1:18760';
  process.env.AUTO_ONTOLOGY_ADMIN_EMAIL = 'fixture@fieldkit.local';
  process.env.AUTO_ONTOLOGY_ADMIN_PASSWORD = 'fixture-password';
  if (tokenOnly) process.env.FIELDKIT_AO_TOKEN_ONLY = '1'; else delete process.env.FIELDKIT_AO_TOKEN_ONLY;
  if (enableOnly) process.env.FIELDKIT_AO_ENABLE_ONLY = '1'; else delete process.env.FIELDKIT_AO_ENABLE_ONLY;
  process.env.AUTO_ONTOLOGY_API_TOKEN = 'auto_ontology_existing_fixture';
  const calls = [];
  let output = '';
  let error;
  globalThis.fetch = async (url, options) => {
    calls.push({ url, options });
    assert.equal(options.headers.Origin, 'http://127.0.0.1:18760');
    if (url.endsWith('/api/auth/sign-in/email')) {
      assert.deepEqual(JSON.parse(options.body), { email: 'fixture@fieldkit.local', password: 'fixture-password' });
      return new Response('{}', { status: 200, headers: { 'set-cookie': 'session=fixture; Path=/; HttpOnly' } });
    }
    if (enableOnly) assert.equal(options.headers['x-api-key'], 'auto_ontology_existing_fixture');
    else assert.equal(options.headers.Cookie, 'session=fixture');
    if (url.endsWith('/api/configurations/semantic-compilation')) {
      assert.equal(options.method, 'PUT');
      assert.deepEqual(JSON.parse(options.body), { enabled: true });
      return new Response('{}', { status: failCompilation ? 503 : 200 });
    }
    assert.ok(url.endsWith('/api/api-tokens'));
    return new Response(JSON.stringify({ token: 'auto_ontology_fixture_token' }), { status: 201 });
  };
  process.stdout.write = value => { output += value; return true; };
  try { await import(`./bootstrap-token.mjs?fixture=${failCompilation}-${tokenOnly}-${enableOnly}`); }
  catch (failure) { error = failure; }
  finally {
    globalThis.fetch = saved.fetch;
    process.stdout.write = saved.write;
    for (const [name, value] of [['APP_URL', saved.origin], ['AUTO_ONTOLOGY_ADMIN_EMAIL', saved.email], ['AUTO_ONTOLOGY_ADMIN_PASSWORD', saved.password], ['FIELDKIT_AO_TOKEN_ONLY', saved.tokenOnly], ['FIELDKIT_AO_ENABLE_ONLY', saved.enableOnly], ['AUTO_ONTOLOGY_API_TOKEN', saved.token]]) {
      if (value === undefined) delete process.env[name]; else process.env[name] = value;
    }
  }
  if (enableOnly) {
    assert.equal(error, undefined);
    assert.equal(calls.length, 1);
    assert.equal(output, 'Semantic compilation enabled\n');
  } else if (failCompilation) {
    assert.ok(error);
    assert.equal(output, '');
    assert.equal(calls.length, 2, 'A compiler failure must not mint an orphan token');
    assert.ok(!error.message.includes('fixture-password'));
  } else {
    assert.equal(error, undefined);
    assert.equal(calls.length, tokenOnly ? 2 : 3);
    assert.ok(output.includes('AUTO_ONTOLOGY_API_TOKEN=auto_ontology_fixture_token\n'));
    assert.ok(output.includes('AUTO_ONTOLOGY_TARGET_DB=fieldkit\n'));
    assert.ok(!output.includes('fixture-password'));
  }
}

await scenario(false);
await scenario(true);
await scenario(false, true);
await scenario(false, false, true);
console.log('Auto Ontology bootstrap contract: 4 scenarios passed');
