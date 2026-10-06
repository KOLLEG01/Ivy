import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const deployPlans = () => ['services', 'ui'].flatMap(owner => readdirSync(owner, { withFileTypes: true })
  .filter(entry => entry.isDirectory()).map(entry => join(owner, entry.name, 'deploy.json')).filter(existsSync));

test('deployment pipelines exclude tests and target only the requested component build', () => {
  const plans = deployPlans().map(path => JSON.parse(readFileSync(path, 'utf8')));
  for (const plan of plans) {
    assert.deepEqual(plan.checks, [], 'Deployment does not run general schema/UI checks; targeted tests run separately.');
    if (plan.kind !== 'app') assert.ok(plan.readiness.command, 'Activation still verifies readiness');
    for (const command of plan.prepare) {
      assert.ok(!command.args.some(arg => /(^|\/)(tests?([/.:-]|$)|build-phone-tests|check-phone-runtime)/.test(arg)), plan.componentId);
    }
  }
  for (const plan of plans) {
    const build = plan.prepare.find(command => command.args.includes('tools/build/build-deployment.mjs'));
    assert.ok(build, plan.componentId + ' must have a deployment build');
    const componentIndex = build.args.indexOf('--component');
    assert.equal(build.args[componentIndex + 1], plan.componentId, plan.componentId + ' must build itself');
    assert.deepEqual(plan.checks, [], plan.componentId + ' must not run a second checked pipeline');
  }
});
