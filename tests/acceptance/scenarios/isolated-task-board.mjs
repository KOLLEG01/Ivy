import assert from 'node:assert/strict';
import { parseArgs } from 'node:util';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { spawn, spawnSync, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { startMixedSoak } from '../support/mixed-soak.mjs';

// Own only this fresh loopback Hive, AgentManager and TaskBoard. Restart TaskBoard at its
// explicit checkpoint without touching the native owner.
const { values } = parseArgs({ options: { ...Object.fromEntries(['distribution', 'native-config', 'auth-file', 'evidence', 'private-distribution', 'secretary-cases', 'quality-case', 'soak-profile'].map(key => [key, { type: 'string' }])), 'native-images': { type: 'boolean' } } });
for (const key of ['distribution', 'native-config', 'auth-file', 'evidence']) assert.ok(values[key], 'Missing --' + key);
assert.equal(Boolean(values['private-distribution']), Boolean(values['secretary-cases']), 'Secretary requires its distribution and fixed cases');
assert.ok(!values['soak-profile'] || values['quality-case'] && !values['native-images'] && !values['secretary-cases'], 'Mixed traffic accompanies the fixed two-turn quality case only');
const secretaryCases = values['secretary-cases'] ? JSON.parse(await readFile(values['secretary-cases'], 'utf8')) : [];
assert.ok(Array.isArray(secretaryCases) && secretaryCases.length <= 6);
const secretaryTurns = secretaryCases.filter(item => !item.expected?.mediaPending).length;
const distribution = resolve(values.distribution), root = resolve(values.evidence);
const load = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
const { HiveServer } = await load('services/hive/src/server.js');
const { startAgentManager } = await load('services/agent-manager/src/main.js');
const { startTaskBoard } = await load('services/task-board/src/main.js');
const { atomicJson } = await load('packages/host-runtime/src/config.js');
const { digest } = await load('packages/contracts/src/canonical.js');
const { HiveClient, discover, callBound } = await load('packages/sdk/src/client.js');
const sourceBytes = await readFile(values['native-config']), authBytes = await readFile(values['auth-file']);
const source = JSON.parse(sourceBytes).instances.find(item => item.componentId === 'agent-manager'); assert.ok(source);
const build = JSON.parse(await readFile(join(distribution, 'dist/build-info.json'), 'utf8'));
await mkdir(root); // Existing evidence or an earlier owner is never silently reused.
if (process.platform === 'win32') {
  const exec = promisify(execFile), identity = await exec('whoami.exe', ['/user', '/fo', 'csv', '/nh'], { windowsHide: true });
  const sid = identity.stdout.match(/S-1-5-[0-9-]+/)?.[0]; assert.ok(sid);
  await exec('icacls.exe', [root, '/inheritance:r', '/grant:r', '*' + sid + ':(OI)(CI)F', '*S-1-5-18:(OI)(CI)F'], { windowsHide: true });
}
const hostId = 'HOST-B-task-board-acceptance', owner = 'acceptance-agent', node = 'acceptance-task-board';
const token = randomUUID(), serviceToken = randomUUID(), principalId = 'acceptance-user';
const project = join(root, 'project'); await mkdir(project);
await writeFile(join(project, 'README.md'), '# Synthetic native acceptance project\n');
const report = { schemaVersion: 1, build, startedAt: new Date().toISOString(), phase: 'starting',
  budget: { model: 'gpt-5.6-luna', effort: 'high', maximumModelTurns: 2 + secretaryTurns + Number(Boolean(values['native-images'])), concurrency: 1, timeoutMs: 600000,
    imageTimeoutMs: values['native-images'] ? 360000 : 0,
    secretaryTimeoutMs: secretaryCases.length ? secretaryTurns * 180000 + (secretaryCases.length - secretaryTurns) * 15000 + 90000 : 0, tokenLimit: null } };
const save = () => atomicJson(join(root, 'report.json'), report);
await save();
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise((yes, no) => reservation.close(error => error ? no(error) : yes()));
const publicBaseUrl = `http://127.0.0.1:${port}/ivy`;
const server = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl, listenHost: '127.0.0.1', listenPort: port,
  consoleRoot: join(distribution, 'dist/console'), version: build.version, buildId: build.buildId,
  credentials: [{ principalId, digest: digest(token) }, { principalId: owner, digest: digest(serviceToken) }] });
let manager, taskBoard, child, childResult, soak;
const readyTaskBoard = async path => {
  const instance = await startTaskBoard(path);
  taskBoard = instance; // Retain ownership for cleanup even when readiness fails.
  await instance.service.waitReady({ timeoutMs: 45000 });
  const deadline = Date.now() + 45000;
  while (!instance.runtime?.recovered && Date.now() < deadline) await delay(100);
  assert.ok(instance.runtime?.recovered, 'TaskBoard must reconcile before the scenario starts');
  return instance;
};
try {
  await server.start();
  const common = { schemaVersion: 1, hostId, publicBaseUrl, artifactRoot: distribution, version: build.version, buildId: build.buildId };
  const agent = { ...common, componentId: 'agent-manager', instanceId: 'agent', serviceNodeId: owner, dataRoot: join(root, 'agent'), credential: serviceToken,
    settings: { nativeExecutable: source.settings.nativeExecutable, nativeVersion: source.settings.nativeVersion,
      nativeExecutableHash: source.settings.nativeExecutableHash, windowsShell: source.settings.windowsShell, nativeHome: join(root, 'agent/native-home'),
      limits: { maxOperations: 1000, maxJournalBytes: 512 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 } } };
  const executor = { ...common, componentId: 'task-board', instanceId: 'task-board', serviceNodeId: node, dataRoot: join(root, 'task-board'), credential: token,
    settings: { principalId, rootObjectId: null, scheduler: { enabled: false, intervalMs: 1000, pageSize: 10 },
       phoneTarget: null } };
  await mkdir(agent.settings.nativeHome, { recursive: true });
  const agentPath = join(root, 'agent.json'), taskBoardPath = join(root, 'task-board.json'), configPath = join(root, 'config.json');
  await atomicJson(agentPath, agent); await atomicJson(taskBoardPath, executor);
  await atomicJson(configPath, { hostId, publicBaseUrl, instances: [{ componentId: 'hive', settings: { credentials: [{ token, principalId }] } }, { componentId: 'agent-manager', serviceNodeId: owner }] });
  manager = await startAgentManager(agentPath); await manager.service.waitReady({ timeoutMs: 45000 });
  const client = new HiveClient(publicBaseUrl, { credential: token });
  const nativeCatalog = JSON.parse(await readFile(join(distribution, 'specs/native/codex-' + source.settings.nativeVersion + '/catalog.json'), 'utf8'));
  if (nativeCatalog.clientRequests.some(item => item.method === 'project/create')) {
    // An owner with project/list exposes its native project store, not settings.projects.
    // Register only this fresh directory in this fresh native home, retaining the original ID.
    const operationId = randomUUID(), params = { idempotencyKey: operationId, name: 'Acceptance', roots: [{ path: project }] };
    report.projectRegistration = { operationId, params, phase: 'intent' }; await save();
    const binding = await discover(client, 'codex.project/create', { serviceNodeId: owner });
    const result = await callBound(client, binding, params, operationId, { timeoutMs: 45000 });
    Object.assign(report.projectRegistration, { phase: 'succeeded', result }); await save();
  }
  const projectBinding = await discover(client, 'agent.projects', { serviceNodeId: owner });
  const projectDeadline = Date.now() + 45000; let observedProject;
  while (Date.now() < projectDeadline) {
    const observation = await callBound(client, projectBinding, {});
    observedProject = observation.projects.find(item => item.paths.includes(project));
    if (observedProject) break;
    await delay(500);
  }
  assert.ok(observedProject, 'Fresh project must appear in the exact native owner inventory');
  report.project = observedProject; await save();
  await readyTaskBoard(taskBoardPath);
  if (values['soak-profile']) soak = await startMixedSoak({ profilePath: resolve(values['soak-profile']), root, client,
    baseUrl: publicBaseUrl, token, owner, taskBoardNode: node, id: 'mixed-' + randomUUID() });
  const scenarioRoot = join(root, 'scenario');
  child = spawn(process.execPath, [join(distribution, 'tests/acceptance/scenarios/task-board-native.mjs'), '--config', configPath, '--host-id', hostId,
    '--owner', owner, '--task-board-node', node, '--project-path', project, '--auth-file', resolve(values['auth-file']), '--evidence', scenarioRoot,
    '--model', 'gpt-5.6-luna', '--effort', 'high', ...(values['quality-case'] ? ['--quality-case', resolve(values['quality-case'])] : [])], { cwd: distribution, windowsHide: true, stdio: ['ignore', 'inherit', 'inherit'] });
  child.once('error', error => { childResult = { error: error.message }; });
  const closed = new Promise(yes => child.once('close', (code, signal) => { childResult = { ...childResult, code, signal }; yes(); }));
  const deadline = Date.now() + report.budget.timeoutMs;
  let restarted = false;
  while (!childResult && Date.now() < deadline) {
    let state;
    try { state = JSON.parse(await readFile(join(scenarioRoot, 'report.json'), 'utf8')); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (!restarted && state?.phase === 'awaiting-task-board-restart') {
      const restartStarted = Date.now(); report.controlledRestart = { startedAt: new Date(restartStarted).toISOString() }; await save();
      await taskBoard.close(); await readyTaskBoard(taskBoardPath);
      Object.assign(report.controlledRestart, { finishedAt: new Date().toISOString(), elapsedMs: Date.now() - restartStarted }); await save();
      restarted = true;
      await atomicJson(join(scenarioRoot, 'control.json'), { action: 'verify-restart', deploymentId: 'isolated-restart-' + randomUUID() });
    }
    await delay(1000);
  }
  assert.ok(childResult, 'Isolated TaskBoard acceptance exceeded its bounded deadline'); await closed;
  assert.equal(childResult.code, 0, 'Native TaskBoard scenario failed'); assert.ok(restarted);
  const scenario = JSON.parse(await readFile(join(scenarioRoot, 'report.json'), 'utf8')); assert.equal(scenario.phase, 'passed');
  Object.assign(report, { phase: 'passed', scenario: join(scenarioRoot, 'report.json'), restarted, childResult });
  if (soak) {
    report.phase = 'mixed-soak-running'; await save();
    const measured = await soak.completion;
    report.mixedSoak = { report: soak.reportPath, phase: measured.phase,
      actualModelTurns: [scenario.nativeTurnId, scenario.continuation?.turnId], scenario: join(scenarioRoot, 'report.json') };
    assert.equal(measured.phase, 'passed', JSON.stringify(measured.failure));
    assert.ok(report.mixedSoak.actualModelTurns.every(Boolean)); assert.equal(new Set(report.mixedSoak.actualModelTurns).size, 2);
    const binding = await discover(client, 'agent.status', { serviceNodeId: owner });
    const final = await callBound(client, binding, {}); assert.equal(final.pendingInputs, 0);
    report.phase = 'passed'; await save();
  }
  if (values['native-images']) {
    report.phase = 'images-starting'; await save();
    const imageEvidence = join(root, 'image-scenario'); childResult = null;
    child = spawn(process.execPath, [join(distribution, 'tests/acceptance/scenarios/native-images.mjs'), '--config', configPath,
      '--owner', owner, '--project', project, '--evidence', imageEvidence, '--model', 'gpt-5.6-luna', '--effort', 'high'],
    { cwd: distribution, windowsHide: true, stdio: ['ignore', 'inherit', 'inherit'] });
    child.once('error', error => { childResult = { error: error.message }; });
    const imageClosed = new Promise(yes => child.once('close', (code, signal) => { childResult = { ...childResult, code, signal }; yes(); }));
    const imageDeadline = Date.now() + report.budget.imageTimeoutMs;
    while (!childResult && Date.now() < imageDeadline) await delay(1000);
    assert.ok(childResult, 'Native image acceptance exceeded its bounded deadline'); await imageClosed;
    assert.equal(childResult.code, 0, 'Native image scenario failed');
    const imageReport = JSON.parse(await readFile(join(imageEvidence, 'report.json'), 'utf8')); assert.equal(imageReport.phase, 'passed');
    Object.assign(report, { phase: 'passed', images: join(imageEvidence, 'report.json') }); await save();
  }
  if (values['private-distribution']) {
    report.phase = 'secretary-starting'; await save();
    const privateRoot = resolve(values['private-distribution']), secretaryEvidence = join(root, 'secretary-scenario');
    childResult = null;
    child = spawn(process.execPath, [join(privateRoot, 'tests/acceptance/scenarios/accept-secretary-synthetic.mjs'), '--host-config', configPath,
      '--owner', owner, '--project-path', project, '--cases', resolve(values['secretary-cases']), '--evidence', secretaryEvidence],
    { cwd: privateRoot, windowsHide: true, stdio: ['ignore', 'inherit', 'inherit'] });
    child.once('error', error => { childResult = { error: error.message }; });
    const secretaryClosed = new Promise(yes => child.once('close', (code, signal) => { childResult = { ...childResult, code, signal }; yes(); }));
    const secretaryDeadline = Date.now() + report.budget.secretaryTimeoutMs;
    while (!childResult && Date.now() < secretaryDeadline) await delay(1000);
    assert.ok(childResult, 'Secretary exceeded its fixed batch deadline'); await secretaryClosed;
    assert.equal(childResult.code, 0, 'Synthetic Secretary scenario failed');
    const secretaryReport = JSON.parse(await readFile(join(secretaryEvidence, 'report.json'), 'utf8'));
    assert.equal(secretaryReport.phase, 'passed'); assert.equal(secretaryReport.items.length, secretaryCases.length);
    Object.assign(report, { phase: 'passed', secretary: join(secretaryEvidence, 'report.json') });
  }
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally {
  try { await soak?.stop(); } catch (error) { report.cleanupError = error.code ?? error.message; process.exitCode = 1; report.phase = 'failed'; }
  if (child?.pid && child.exitCode === null && child.signalCode === null) {
    if (process.platform === 'win32') spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 15000, stdio: 'ignore' });
    else child.kill('SIGKILL');
  }
  for (const owned of [taskBoard, manager, server]) try { await owned?.close(); } catch (error) { report.cleanupError = error.code ?? error.message; process.exitCode = 1; report.phase = 'failed'; }
  report.sourceConfigUnchanged = sourceBytes.equals(await readFile(values['native-config']));
  report.sourceAuthUnchanged = authBytes.equals(await readFile(values['auth-file']));
  if (!report.sourceConfigUnchanged || !report.sourceAuthUnchanged) { process.exitCode = 1; report.phase = 'failed'; }
  report.finishedAt = new Date().toISOString(); await save();
  console.log(JSON.stringify({ phase: report.phase, report: join(root, 'report.json'), failure: report.failure }));
}
