// Control the actual gateway's scheduler flag. This never deletes a catalog or
// compiled semantic layer, and it never asks a model a question.
export function compilationComplete(status, since) {
  const success = Date.parse(status.last_success_at ?? '');
  return status.calculated === true && status.running === false &&
    Number.isFinite(success) && success >= since && !status.last_failure_at;
}

export async function control({ env = process.env, fetchImpl = globalThis.fetch,
  sleep = ms => new Promise(resolve => setTimeout(resolve, ms)), clock = Date.now,
  progress = message => console.error(message) } = {}) {
  const origin = env.APP_URL;
  if (!/^http:\/\/127\.0\.0\.1:[0-9]+$/.test(origin ?? '')) throw new Error('Local semantic control requires a loopback APP_URL');
  const base = 'http://127.0.0.1:3000';
  const action = env.FIELDKIT_AO_CONTROL_ACTION;
  if (!['wait', 'freeze'].includes(action)) throw new Error('Semantic control action must be wait or freeze');
  let headers = { Origin: origin };
  if (env.AUTO_ONTOLOGY_API_TOKEN) headers['x-api-key'] = env.AUTO_ONTOLOGY_API_TOKEN;
  else {
    if (action !== 'freeze') throw new Error('Waiting for compilation requires the saved client token');
    // Cleanup must work even if bootstrap failed before creating the token.
    const login = await fetchImpl(base + '/api/auth/sign-in/email', {
      method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' },
      body: JSON.stringify({ email: env.AUTO_ONTOLOGY_ADMIN_EMAIL, password: env.AUTO_ONTOLOGY_ADMIN_PASSWORD }),
      signal: AbortSignal.timeout(30000),
    });
    if (!login.ok) throw new Error(`Cleanup sign-in failed (${login.status}); the ingestion container will still be stopped`);
    const cookie = login.headers.getSetCookie().map(value => value.split(';', 1)[0]).join('; ');
    if (!cookie) throw new Error('Cleanup sign-in returned no session cookie');
    headers.Cookie = cookie;
  }
  if (action === 'freeze') {
    const response = await fetchImpl(base + '/api/configurations/semantic-compilation', {
      method: 'PUT', headers: { ...headers, 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: false }), signal: AbortSignal.timeout(30000),
    });
    if (!response.ok) throw new Error(`Disabling periodic compilation failed (${response.status}); the ingestion container will still be stopped`);
    const result = await response.json();
    if (result.enabled !== false) throw new Error('Gateway did not confirm periodic compilation was disabled');
    progress('Periodic compilation disabled; retained catalog and semantic layer are unchanged');
    return result;
  }
  const timeout = Number(env.FIELDKIT_AO_SETUP_TIMEOUT ?? 1800);
  if (!Number.isInteger(timeout) || timeout < 1 || timeout > 7200) throw new Error('Setup timeout must be 1–7200 seconds');
  const since = Date.parse(env.FIELDKIT_AO_COMPILE_SINCE ?? '');
  if (!Number.isFinite(since)) throw new Error('Compilation wait requires its request timestamp');
  const deadline = clock() + timeout * 1000;
  let lastState;
  let lastNotice = 0;
  while (clock() < deadline) {
    const response = await fetchImpl(base + '/api/semantic-compilation/status', {
      headers, signal: AbortSignal.timeout(30000),
    });
    if (!response.ok) throw new Error(`Compilation status failed (${response.status})`);
    const status = await response.json();
    const state = JSON.stringify(status);
    if (compilationComplete(status, since)) {
      progress('Initial semantic compilation completed successfully');
      return status;
    }
    const failed = Date.parse(status.last_failure_at ?? '');
    if (Number.isFinite(failed) && failed >= since) throw new Error('The semantic compiler reported a failed pass; inspect ingestion-service logs before retrying');
    if (state !== lastState || clock() - lastNotice >= 30000) {
      progress(status.running ? 'Initial semantic compilation is running' : 'Waiting for initial catalog embedding and semantic compilation');
      lastState = state;
      lastNotice = clock();
    }
    await sleep(5000);
  }
  throw new Error('Initial compilation timed out; background compilation and ingestion will be stopped');
}
