// Native Windows compilers do not consistently accept verbatim temporary paths.
export function compilerPath(path, platform = process.platform) {
  if (platform !== 'win32' || typeof path !== 'string') return path;
  if (path.startsWith('\\\\?\\UNC\\')) return '\\\\' + path.slice(8);
  if (/^\\\\\?\\[A-Za-z]:\\/.test(path)) return path.slice(4);
  return path;
}

export function compilerEnvironment(environment, platform = process.platform) {
  return Object.fromEntries(Object.entries(environment).map(([key, value]) =>
    [key, /^(TEMP|TMP|TMPDIR)$/i.test(key) ? compilerPath(value, platform) : value]));
}
