/** Agent 给出的 file URI 和相对引用指向 Host。不得注册为客户端本地路径。 */
export function hostFilePath(reference: string, base?: string): string {
  const absolute = reference.startsWith("/");
  const url = reference.startsWith("file:")
    ? new URL(reference)
    : absolute
      ? new URL(hostFileUri(reference))
      : base
        ? new URL(reference, hostFileUri(base))
        : null;
  if (
    !url ||
    url.protocol !== "file:" ||
    (url.hostname && url.hostname !== "localhost") ||
    url.search
  )
    throw new Error("Host 文件路径无效。");
  const path = decodeURIComponent(url.pathname);
  if (!path.startsWith("/") || path.includes("\0"))
    throw new Error("Host 文件路径无效。");
  return path;
}
export function hostFileUri(path: string): string {
  return `file://${path.split("/").map(encodeURIComponent).join("/")}`;
}
