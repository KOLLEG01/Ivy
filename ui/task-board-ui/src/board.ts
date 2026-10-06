import { Circle, CircleCheck, CircleDashed, CircleDot, CircleEllipsis, CirclePause, CircleX } from '@lucide/vue';
import { tr, type TaskBoard } from './runtime';

export const boardColumns = [
  { id: 'todo', title: 'Todo', states: ['todo'] },
  { id: 'in_progress', title: 'In progress', states: ['in_progress'] },
  { id: 'waiting', title: 'Blocked', states: ['waiting'] },
  { id: 'review', title: 'Review', states: ['review'] },
  { id: 'done', title: 'Done', states: ['done', 'cancelled'] },
];
/** Matches TaskBoard: after any attempt, claim, Run or native context the host and project are fixed. */
export const taskStarted = (task: TaskBoard.Task) =>
  task.attemptCount > 0 || !!task.claim || !!task.lastRun || !!task.primaryResourceRef;
export type BoardTransition = Exclude<TaskBoard.Task['workflowState'], 'review'>;
export interface TransitionContext { workflowState: TaskBoard.Task['workflowState']; control: TaskBoard.TaskFields['control']; claimed: boolean; waiting: string; hasResult: boolean; publishing: boolean }
export const taskContext = (task: TaskBoard.Task): TransitionContext => ({ workflowState: task.workflowState, control: task.fields.control, claimed: !!task.claim, waiting: task.waiting?.reason ?? '', hasResult: !!task.latestResult, publishing: !!task.publication });
export function transitions(task: TransitionContext): Array<{ action: BoardTransition; label: string }> {
  if (task.publishing || task.claimed || task.waiting === 'external_outcome' || task.workflowState === 'cancelled') return [];
  const result: Array<{ action: BoardTransition; label: string }> = [];
  if (['backlog', 'waiting', 'review', 'done'].includes(task.workflowState)) result.push({ action: 'todo', label: 'Move to Todo' });
  if (task.control === 'user' && task.workflowState === 'todo') result.push({ action: 'in_progress', label: 'Start manual work' });
  if (['todo', 'in_progress'].includes(task.workflowState)) result.push({ action: 'waiting', label: 'Mark blocked' });
  if (['in_progress', 'waiting', 'review'].includes(task.workflowState)) result.push({ action: 'done', label: 'Move to Done' });
  if (task.workflowState !== 'backlog') result.push({ action: 'backlog', label: 'Move to Backlog' });
  if (task.workflowState !== 'done') result.push({ action: 'cancelled', label: 'Cancel task' });
  return result;
}

export type Tone = 'good' | 'bad' | 'warning' | 'neutral';
const conditionLabels: Record<string, string> = {
  blocked_environment: 'Environment unavailable', blocked_dependency: 'Blocked', needs_user: 'Needs you', outcome_unknown: 'Recovery required',
  publishing: 'Publishing', starting: 'Starting', active: 'Working', deferred: 'Scheduled', queued: 'Queued', idle: 'Not scheduled',
};
export const conditionLabel = (condition: string) => condition === 'recovering' ? tr('Statusabgleich', 'Reconciling status') : conditionLabels[condition] ?? condition.replaceAll('_', ' ');
export const conditionTone = (condition: string): Tone =>
  condition.startsWith('blocked') || condition === 'outcome_unknown' ? 'bad' : ['needs_user', 'publishing', 'starting', 'recovering'].includes(condition) ? 'warning' : condition === 'active' ? 'good' : 'neutral';
/** Cards only surface conditions that ask for attention; the column already states ordinary progress. */
export const noteworthyCondition = (condition: string) => !['idle', 'active', 'queued'].includes(condition);
export const stateLabels: Record<string, string> = { backlog: 'Backlog', todo: 'Todo', in_progress: 'In progress', waiting: 'Blocked', review: 'Review', done: 'Done', cancelled: 'Cancelled' };
export const stateLabel = (state: string) => stateLabels[state] ?? state.replaceAll('_', ' ');
export const priorityLabels: Record<number, string> = { 4: 'Urgent', 3: 'High', 2: 'Medium', 1: 'Low', 0: 'Lowest' };
export const priorityLabel = (priority: unknown) => typeof priority === 'number' ? priorityLabels[priority] ?? 'P' + priority : 'None';
export const shortDate = (value: string) => new Date(value).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
export const stateIcons = { backlog: CircleDashed, todo: Circle, in_progress: CircleDot, waiting: CirclePause, review: CircleEllipsis, done: CircleCheck, cancelled: CircleX } as Record<string, typeof Circle>;
export const stateColors: Record<string, string> = { in_progress: 'text-amber-500', review: 'text-sky-500', done: 'text-emerald-500', cancelled: 'text-muted-foreground' };
/** Where a backlog Task is ranked: before another Task or at the end (null), after another Task, or first. */
export type Placement = { before: string | null } | { after: string } | { top: true };
