import { clsx } from 'clsx';
import type { ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]): string { return twMerge(clsx(inputs)); }

/** Forward only present values to primitives; optional Vue props otherwise materialize as undefined. */
export type DefinedProps<T> = { [K in keyof T as undefined extends T[K] ? never : K]: T[K] }
  & { [K in keyof T as undefined extends T[K] ? K : never]?: Exclude<T[K], undefined> };
export function definedProps<T extends object>(value: T): DefinedProps<T> {
  return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined)) as DefinedProps<T>;
}
