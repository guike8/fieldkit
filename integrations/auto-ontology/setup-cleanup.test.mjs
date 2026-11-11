import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const original = path.dirname(fileURLToPath(import.meta.url));
for (const waitFails of [false, true]) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'fieldkit-setup-cleanup-'));
  try {
    const integration = path.join(directory, 'integrations', 'auto-ontology');
    const bin = path.join(directory, 'bin');
    fs.mkdirSync(path.join(integration, 'data'), { recursive: true });
    fs.mkdirSync(bin);
    for (const name of ['setup.sh', 'semantic-control.mjs', 'bootstrap-token.mjs']) fs.copyFileSync(path.join(original, name), path.join(integration, name));
    fs.writeFileSync(path.join(integration, '.env'), 'NVIDIA_API_KEY=fixture-key\nAUTO_ONTOLOGY_SOURCE=/fixture/source\nFIELDKIT_PROJECTION_DIR=' + path.join(integration, 'data') + '\n', { mode: 0o600 });
    fs.writeFileSync(path.join(integration, 'fieldkit.env'), 'AUTO_ONTOLOGY_API_TOKEN=fixture-token\n', { mode: 0o600 });
    fs.writeFileSync(path.join(integration, 'data', 'fieldkit.sqlite'), 'fixture');
    fs.writeFileSync(path.join(bin, 'git'), '#!/bin/sh\ncase "$*" in *rev-parse*) printf "%s\\n" fc43a82600e18b19d159c52023f54f5572f537ac;; esac\n', { mode: 0o700 });
    fs.writeFileSync(path.join(bin, 'docker'), `#!/bin/sh
printf '%s\\n' "$*" >> "$FIELDKIT_DOCKER_LOG"
case " $* " in
  *" exec "*)
    cat > /dev/null
    case " $* " in *" FIELDKIT_AO_ENABLE_ONLY=1 "*) exit 0;; esac
    if [ "$FIELDKIT_AO_CONTROL_ACTION" = wait ] && [ "$FIELDKIT_FIXTURE_WAIT_FAILS" = 1 ]; then exit 7; fi
    ;;
esac
exit 0
`, { mode: 0o700 });
    const log = path.join(directory, 'docker.log');
    const result = spawnSync('/bin/sh', [path.join(integration, 'setup.sh'), 'connect'], {
      env: { ...process.env, PATH: bin + path.delimiter + process.env.PATH, FIELDKIT_DOCKER_LOG: log,
        FIELDKIT_FIXTURE_WAIT_FAILS: waitFails ? '1' : '0', FIELDKIT_AO_SETUP_TIMEOUT: '1' }, encoding: 'utf8',
    });
    assert.equal(result.status, waitFails ? 7 : 0, result.stderr);
    const calls = fs.readFileSync(log, 'utf8').trim().split('\n');
    assert.ok(calls.at(-1).endsWith('stop ingestion-service'), 'Every completion path must stop the periodic worker');
    assert.ok(calls.filter(value => value.endsWith('stop ingestion-service')).length >= 2, 'Worker is frozen before and after the one-time pass');
    assert.ok(!result.stdout.includes('fixture-token'));
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
}
console.log('Setup cleanup: success and failed-compilation paths stop the worker');
