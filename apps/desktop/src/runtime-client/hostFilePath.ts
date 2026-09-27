/** 路径语法属于 Host，不能根据浏览器所在系统选择。UNC/设备路径继续交由既有边界拒绝。 */
export function hostFilePath(reference: string, base?: string): string {
  let url: URL;
  if (/^file:/i.test(reference)) {
    url = new URL(reference);
  } else if (isAbsoluteHostPath(reference)) {
    url = new URL(hostFileUri(reference));
  } else {
    if (!base || reference.startsWith("\\") || reference.startsWith("//") || /^[a-z][a-z\d+.-]*:/i.test(reference)) throw invalidPath();
    const base_uri = hostFileUri(base);
    // Windows 文档内的反斜杠相对引用与正斜杠相同；POSIX 文件名中的反斜杠不是目录分隔符。
    const relative = /^file:\/\/\/[a-z]:\//i.test(base_uri) ? reference.replaceAll("\\", "/") : reference;
    url = new URL(relative, base_uri);
  }
  if (url.protocol !== "file:" || (url.hostname && url.hostname !== "localhost")
    || url.search || url.hash || /%2f|%5c/i.test(url.pathname)) throw invalidPath();
  const path = decodeURIComponent(url.pathname);
  if (!path.startsWith("/") || path.includes("\0")) throw invalidPath();
  // file URI 的首个 / 不属于 Windows 盘符路径。
  return /^\/[a-z]:\//i.test(path) ? path.slice(1) : path;
}

export function isAbsoluteHostPath(path: string): boolean {
  return (path.startsWith("/") && !path.startsWith("//"))
    || /^[a-z]:[/\\]/i.test(path) || /^\\\\\?\\[a-z]:\\/i.test(path);
}

export function hostFileUri(path: string): string {
  if (!isAbsoluteHostPath(path) || path.includes("\0")) throw invalidPath();
  const normalized = path.replace(/^\\\\\?\\(?=[a-z]:\\)/i, "");
  if (/^[a-z]:[/\\]/i.test(normalized)) {
    const [drive, ...parts] = normalized.replaceAll("\\", "/").split("/");
    return `file:///${drive}/${parts.map(encodeURIComponent).join("/")}`;
  }
  return `file://${normalized.split("/").map(encodeURIComponent).join("/")}`;
}

function invalidPath(): Error {
  return new Error("Host 文件路径无效。");
}
