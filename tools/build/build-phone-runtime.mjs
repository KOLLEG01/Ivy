import {copyFileSync, constants, existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {buildIdentity} from '../../packages/host-runtime/src/build-identity.mjs';
import {materializeNativePlan} from '../../packages/host-runtime/src/native-plan.mjs';
import {compileRustBackend, compileRustHelpers} from './build-rust-backend.mjs';
import {stageNativePayloads} from './stage-native-payloads.mjs';
import {installPhoneEvs, preparePhoneEvs} from '../package/phone-evs-package.mjs';

const windowsTarget = 'x86_64-pc-windows-msvc';
const notices = {
  SIPSorcery: 'SIPSorcery-10.0.15.md', CodexMicro: 'CodexMicro-MIT.txt',
  WaitingMarimba: 'WaitingMarimba.txt', VSCO2: 'VSCO-2-CE-CC0.txt',
  NAudio: 'NAudio-MIT.txt',
};
const hash = bytes => 'sha256:' + createHash('sha256').update(bytes).digest('hex');

/** Keep the existing configured companion path and Job helper path. Both are
 * copies of the selected Rust payload; no CLR/input DLL or fallback is shipped. */
export async function stagePhoneCompatibility({root, outputRoot = join(root, 'dist'), platformDirectory, evs}) {
  const destination = join(outputRoot, 'native/phone');
  if (existsSync(destination)) throw Error('Phone runtime requires a fresh compatibility destination.');
  const native = readFileSync(join(platformDirectory, 'ivy-phone-native.exe'));
  if (!native.length) throw Error('The selected Rust Phone companion is missing.');
  mkdirSync(destination);
  const executable = join(destination, 'Ivy.PhoneRuntime.exe');
  writeFileSync(executable, native, {flag:'wx'});
  await installPhoneEvs(root, destination, evs);
  mkdirSync(join(destination, 'licenses'), {recursive:true});
  for (const [name, source] of Object.entries(notices)) {
    copyFileSync(join(platformDirectory, 'LICENSE.' + name), join(destination, 'licenses', source), constants.COPYFILE_EXCL);
  }
  copyFileSync(join(platformDirectory, 'LICENSE.Ivy'), join(destination, 'LICENSE'), constants.COPYFILE_EXCL);
  copyFileSync(join(platformDirectory, 'ivy-job.exe'), join(outputRoot, 'native/ivy-job.exe'), constants.COPYFILE_EXCL);
  return {executable, executableHash:hash(native), evsLibraryHash:'sha256:' + evs.report.librarySha256};
}

/** Normal preparation builds one real Windows target, stages the shared native
 * service/health/Job payload, then adds Phone's retained configuration layout. */
export async function buildPhoneRuntime({root = process.cwd(), outputRoot, target = windowsTarget, identity, cargo, evsReportPath} = {}) {
  if (process.platform !== 'win32' || process.arch !== 'x64' || target !== windowsTarget)
    throw Error('Phone native preparation requires Windows x64.');
  root = realpathSync(root);
  outputRoot = resolve(outputRoot ?? join(root, 'dist'));
  identity ??= buildIdentity(root);
  const source = JSON.parse(readFileSync(join(root, 'services/phone-bridge/deploy.json'), 'utf8'));
  const plan = materializeNativePlan(source, {os:'win32', arch:'x64'});
  const evs = await preparePhoneEvs(root, evsReportPath);
  // Shared build inputs are contained in the immutable source snapshot even
  // when the validated dependency was reused from an external compiler cache.
  const dependencies = join(root, '.local/build/phone-runtime-dependencies');
  mkdirSync(dependencies, {recursive:true});
  const runtimeFiles = [];
  for (const [name, bytes] of [
    ['ivy_phone_evs.dll', evs.library],
    ['LICENSE.EVS', readFileSync(join(root, 'services/phone-bridge/native/evs/NOTICE.md'))],
    ['LICENSE.Ivy', readFileSync(join(root, 'LICENSE'))],
    ...Object.entries(notices).map(([name, file]) => ['LICENSE.' + name,
      readFileSync(join(root, 'services/phone-bridge/assets/licenses', file))]),
  ]) {
    const path = join(dependencies, name);
    writeFileSync(path, bytes); runtimeFiles.push(path);
  }
  const payload = compileRustBackend({component:'phone-bridge', target, root, identity, cargo, runtimeFiles});
  const helpers = compileRustHelpers({component:'host-executor', target, binaries:['ivy-health','ivy-job'], root, identity, cargo});
  const staged = stageNativePayloads({component:'phone-bridge', root, outputRoot, identity, payloads:[payload,helpers], plan,
    requiredBinaries:{win32:['ivy-health','ivy-job']}});
  const compatibility = await stagePhoneCompatibility({root, outputRoot,
    platformDirectory:join(outputRoot, 'native/win32-x64'), evs});
  return {...staged, ...compatibility, plan, staged:true};
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const index = process.argv.indexOf('--target');
  const result = await buildPhoneRuntime({target:index < 0 ? windowsTarget : process.argv[index + 1]});
  process.stdout.write(JSON.stringify({component:result.component, ...result.stamp,
    executableHash:result.executableHash, evsLibraryHash:result.evsLibraryHash}) + '\n');
}
