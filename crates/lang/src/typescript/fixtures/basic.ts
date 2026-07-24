export function greet(name: string): string {
  return `hi ${name}`;
}

const helper = (x: number) => x * 2;

export class Widget {
  private count: number = 0;

  increment(): void {
    this.count += 1;
  }
}

export interface Options {
  verbose: boolean;
}

export type Id = string;

export enum Color {
  Red,
  Green,
}
