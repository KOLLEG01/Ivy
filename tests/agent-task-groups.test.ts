import test from 'node:test';
import assert from 'node:assert/strict';
import type { Agent, Operation } from '../packages/contracts/src/generated.js';
import { groupTasks, isVisibleTask, normalizedPath, projectChoices, partitionInternalTasks } from '../ui/agent-ui/src/task-groups.js';

const project = (nativeId: string, name: string, paths: string[], source: Agent.ProjectSummary['source'] = 'native'): Agent.ProjectSummary =>
  ({ nativeId, name, paths, source });
const task = (nativeId: string, cwd: string, projectId: string | null | undefined): Operation.InventoryItem => ({
  resourceRef: { serviceNodeId: 'node', namespace: 'codex', kind: 'thread', nativeId }, schemaVersion: '1.0.0',
  summary: { nativeId, cwd, ...(projectId === undefined ? {} : { projectId }) }, observedAt: '2026-09-14T00:00:00.000Z', stale: false, snapshotRevision: 1,
});

test('task navigation excludes native guardian sessions by source while preserving user tasks and other agents', () => {
  const sources = [
    { subAgent: { other: 'guardian' } },
    'vscode',
    { subAgent: { thread_spawn: { parent_thread_id: 'parent', depth: 1 } } },
    { subAgent: { other: 'another-agent' } },
    null,
  ];
  const tasks = sources.map((source, index) => ({ ...task(String(index), '/fixture', null),
    summary: { source, name: 'Guardian review', preview: 'Approval review' } }));
  assert.deepEqual(tasks.filter(isVisibleTask).map(value => value.resourceRef.nativeId), ['1', '2', '3', '4']);
  assert.equal(isVisibleTask({ ...task('service-context', '/fixture', null), summary: { ephemeral: true, name: null, preview: '' } }), false,
    'ephemeral service contexts have no saved history to open, archive or delete');
});

test('native project identity keeps an external worktree in its Desktop project', () => {
  const groups = groupTasks([project('ivy', 'Ivy', ['C:\\projects\\sample-project'])], [task('worktree', 'C:\\projects\\sample-project-desktop-task-sync', 'ivy')]);
  assert.deepEqual(groups.map(group => [group.id, group.tasks.map(value => value.resourceRef.nativeId)]), [['ivy', ['worktree']]]);
});

test('fallback grouping normalizes Windows spelling and selects the longest containing root', () => {
  const groups = groupTasks([project('root', 'Root', ['C:\\PROJECTS']), project('ivy', 'Ivy', ['D:/unused', 'c:/projects/sample-project'])], [task('nested', 'C:/Projects/sample-project/ui/agent-ui', undefined)]);
  assert.equal(groups.find(group => group.id === 'ivy')?.tasks[0]?.resourceRef.nativeId, 'nested');
  assert.equal(normalizedPath('C:\\PROJECTS\\sample-project\\'), 'c:/projects/sample-project');
  assert.equal(normalizedPath('\\\\SERVER\\Share\\Work\\'), '//server/share/work');
});

test('native projects with the same root retain their distinct identities', () => {
  const projects = [project('first', 'First', ['/shared']), project('second', 'Second', ['/shared'])];
  const groups = groupTasks(projects, [task('one', '/shared', 'first'), task('two', '/shared', 'second')]);
  assert.deepEqual(groups.map(group => [group.id, group.tasks.map(item => item.resourceRef.nativeId)]), [['first', ['one']], ['second', ['two']]]);
  assert.equal(projectChoices(projects).length, 2);
});

test('IvyInternal tasks are bundled per host and native membership while other projects remain in Recent', () => {
  const projects = [
    { serviceNodeId: 'node', project: project('internal', 'IvyInternal', ['C:/internal']) },
    { serviceNodeId: 'node', project: project('nested', 'User project', ['C:/internal/user']) },
    { serviceNodeId: 'other-host', project: project('internal', 'User project', ['/elsewhere']) },
  ];
  const tasks = [task('service', 'D:/external-worktree', 'internal'), task('old-service', 'c:\\internal\\run', undefined),
    task('user', 'C:/internal/user/task', 'nested'), task('removed-project', 'C:/internal', 'deleted'),
    { ...task('other-host-user', '/elsewhere', 'internal'), resourceRef: { ...task('x', '', null).resourceRef, serviceNodeId: 'other-host', nativeId: 'other-host-user' } }];
  const split = partitionInternalTasks(projects, tasks);
  assert.deepEqual(split.internal.map(value => value.resourceRef.nativeId), ['service', 'old-service']);
  assert.deepEqual(split.recent.map(value => value.resourceRef.nativeId), ['user', 'removed-project', 'other-host-user']);
});

test('explicitly unassigned tasks remain outside containing projects and internal groups', () => {
  const internal = project('internal', 'IvyInternal', ['/internal']);
  const tasks = [task('unassigned', '/internal/unassigned/one', null), task('cleared', '/internal/task', '')];
  const groups = groupTasks([internal], tasks);
  assert.equal(groups[0]?.tasks.length, 0);
  assert.deepEqual(groups[1]?.tasks, tasks);
  assert.equal(groups[1]?.name, 'No project');
  assert.deepEqual(partitionInternalTasks([{ serviceNodeId: 'node', project: internal }], tasks), { recent: tasks, internal: [] });
});
