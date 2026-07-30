export function getPath(url: string): string {
  return url.split("?")[0];
}
