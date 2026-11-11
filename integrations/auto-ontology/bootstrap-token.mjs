// Runs inside the official frontend image. Secrets are read from container env;
// the generated token is written only to stdout redirected to a private file.
const origin = process.env.APP_URL;
if (!/^http:\/\/127\.0\.0\.1:[0-9]+$/.test(origin ?? '')) {
  throw new Error('The local bootstrap requires a loopback APP_URL');
}
const base = 'http://127.0.0.1:3000';
const tokenOnly = process.env.FIELDKIT_AO_TOKEN_ONLY === '1';
const enableOnly = process.env.FIELDKIT_AO_ENABLE_ONLY === '1';
const email = process.env.AUTO_ONTOLOGY_ADMIN_EMAIL;
const password = process.env.AUTO_ONTOLOGY_ADMIN_PASSWORD;
if (!enableOnly && (!email || !password)) throw new Error('Bootstrap admin credentials are not configured');

async function enableCompilation(headers) {
  const response = await fetch(base + '/api/configurations/semantic-compilation', {
    method: 'PUT', headers: { 'Content-Type': 'application/json', Origin: origin, ...headers },
    body: JSON.stringify({ enabled: true }), signal: AbortSignal.timeout(30000),
  });
  if (!response.ok) throw new Error(`Enabling semantic compilation failed (${response.status}); enable it in Settings`);
}

if (enableOnly) {
  const token = process.env.AUTO_ONTOLOGY_API_TOKEN;
  if (!token) throw new Error('An existing client token is required');
  await enableCompilation({ 'x-api-key': token });
  process.stdout.write('Semantic compilation enabled\n');
} else {

async function request(path, body, cookie) {
  const response = await fetch(base + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: origin, ...(cookie ? { Cookie: cookie } : {}) },
    body: JSON.stringify(body),
    redirect: 'manual',
    signal: AbortSignal.timeout(30000),
  });
  if (!response.ok) throw new Error(`Auto Ontology bootstrap failed (${response.status}) at ${path}; no credentials were printed`);
  return response;
}

const login = await request('/api/auth/sign-in/email', { email, password });
const cookies = login.headers.getSetCookie().map(value => value.split(';', 1)[0]).join('; ');
if (!cookies) throw new Error('Sign-in returned no session cookie');
// Enable compilation before minting a token, so a failed setup does not leave
// behind an unused credential that was never written to the client file.
if (!tokenOnly) await enableCompilation({ Cookie: cookies });
const created = await request('/api/api-tokens', { name: 'Fieldkit local evidence reasoning', expires_in_days: 365 }, cookies);
const { token } = await created.json();
if (typeof token !== 'string' || !/^[A-Za-z0-9_.\/=+-]+$/.test(token)) throw new Error('Token response was invalid');
process.stdout.write(`AUTO_ONTOLOGY_API_URL=${origin}\nAUTO_ONTOLOGY_API_TOKEN=${token}\nAUTO_ONTOLOGY_TARGET_DB=fieldkit\nAUTO_ONTOLOGY_TIMEOUT=300\n`);
}
