import { createHash, createPublicKey, verify } from 'node:crypto';
import { IdentityError } from '../identity/errors.js';

export const desktopTargets = ['windows-x86_64', 'windows-aarch64', 'darwin-aarch64'] as const;
export type DesktopTarget = typeof desktopTargets[number];
export type Publication = 'draft' | 'published' | 'withdrawn';
export type ReleaseFile = { name: string; purposes: ('installer' | 'updater')[]; size: number; sha256: string; signature?: string };
export type ReleaseManifest = {
  product: 'ez-assistant'; version: string; min_supported_version: string; target: DesktopTarget;
  host: { version: string; min_supported_version: string; enterprise_protocols: number[] };
  files: ReleaseFile[];
};
export const fileLimit = 512 * 1024 * 1024;
export const invalid = () => new IdentityError(400, 'DESKTOP_RELEASE_INVALID');

export function object(value: unknown, keys: string[]): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !keys.includes(key))) throw invalid();
  return value as Record<string, unknown>;
}

export function version(value: unknown): string {
  if (typeof value !== 'string' || !/^(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})$/.test(value)) throw invalid();
  return value;
}

export function compareVersions(a: string, b: string): number {
  const left = a.split('.').map(Number), right = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) if (left[i] !== right[i]) return left[i]! - right[i]!;
  return 0;
}

export function target(value: unknown): DesktopTarget {
  if (!desktopTargets.includes(value as DesktopTarget)) throw invalid();
  return value as DesktopTarget;
}

export function limitedText(value: unknown, bytes: number): string {
  if (typeof value !== 'string' || Buffer.byteLength(value) > bytes || value.includes('\0')) throw invalid();
  return value;
}

function base64(text: string): Buffer {
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(text)) throw invalid();
  const decoded = Buffer.from(text, 'base64');
  if (decoded.toString('base64') !== text) throw invalid();
  return decoded;
}

/** Tauri signer 使用外层 base64 包裹 Minisign 文本；仅解析格式，密码运算全部委托 Node crypto。 */
function lines(value: string): string[] {
  const text = value.trim().startsWith('untrusted comment:') ? value.trim() : base64(value.trim()).toString('utf8').trim();
  return text.split(/\r?\n/);
}

export function verifyReleaseSignature(content: Uint8Array, signature: string, publicKey: string): void {
  try {
    const keyLines = lines(publicKey), sigLines = lines(signature);
    if (keyLines.length !== 2 || sigLines.length !== 4 || !keyLines[0]!.startsWith('untrusted comment:')
      || !sigLines[0]!.startsWith('untrusted comment:') || !sigLines[2]!.startsWith('trusted comment: ')) throw invalid();
    const key = base64(keyLines[1]!), sig = base64(sigLines[1]!), global = base64(sigLines[3]!);
    if (key.length !== 42 || sig.length !== 74 || global.length !== 64 || key.subarray(0, 2).toString() !== 'Ed'
      || !sig.subarray(2, 10).equals(key.subarray(2, 10))) throw invalid();
    const algorithm = sig.subarray(0, 2).toString();
    if (algorithm !== 'ED' && algorithm !== 'Ed') throw invalid();
    const data = algorithm === 'ED' ? createHash('blake2b512').update(content).digest() : content;
    const spki = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), key.subarray(10)]), format: 'der', type: 'spki' });
    if (!verify(null, data, spki, sig.subarray(10))
      || !verify(null, Buffer.concat([sig.subarray(10), Buffer.from(sigLines[2]!.slice(17))]), spki, global)) throw invalid();
  } catch { throw new IdentityError(400, 'DESKTOP_RELEASE_SIGNATURE_INVALID'); }
}

/** 只在接纳签名小清单时严格解析；不读取、解包或执行制品。 */
export function parseManifest(text: string): ReleaseManifest {
  limitedText(text, 64 * 1024);
  let input: unknown;
  try { input = JSON.parse(text); } catch { throw invalid(); }
  const m = object(input, ['product', 'version', 'min_supported_version', 'target', 'host', 'files']);
  if (m.product !== 'ez-assistant') throw invalid();
  const host = object(m.host, ['version', 'min_supported_version', 'enterprise_protocols']);
  if (!Array.isArray(host.enterprise_protocols) || !host.enterprise_protocols.length || host.enterprise_protocols.length > 16
    || host.enterprise_protocols.some(p => !Number.isSafeInteger(p) || p < 1 || p > 65535)
    || new Set(host.enterprise_protocols).size !== host.enterprise_protocols.length) throw invalid();
  if (!Array.isArray(m.files) || !m.files.length || m.files.length > 2) throw invalid();
  const files = m.files.map(value => {
    const f = object(value, ['name', 'purposes', 'size', 'sha256', 'signature']);
    if (typeof f.name !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,180}$/.test(f.name)
      || !Number.isSafeInteger(f.size) || Number(f.size) < 1 || Number(f.size) > fileLimit
      || typeof f.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(f.sha256)
      || !Array.isArray(f.purposes) || !f.purposes.length || f.purposes.length > 2
      || f.purposes.some(p => !['installer', 'updater'].includes(p))) throw invalid();
    const signature = f.signature === undefined ? undefined : limitedText(f.signature, 8192);
    if (f.purposes.includes('updater') !== Boolean(signature)) throw invalid();
    return { name: f.name, purposes: f.purposes as ReleaseFile['purposes'], size: Number(f.size), sha256: f.sha256, ...(signature ? { signature } : {}) };
  });
  const purposes = files.flatMap(f => f.purposes);
  if (purposes.length !== 2 || new Set(purposes).size !== 2 || new Set(files.map(f => f.name)).size !== files.length) throw invalid();
  const result: ReleaseManifest = { product: 'ez-assistant', version: version(m.version), min_supported_version: version(m.min_supported_version), target: target(m.target),
    host: { version: version(host.version), min_supported_version: version(host.min_supported_version), enterprise_protocols: host.enterprise_protocols }, files };
  if (compareVersions(result.min_supported_version, result.version) > 0 || compareVersions(result.host.min_supported_version, result.host.version) > 0
    || compareVersions(result.version, result.host.min_supported_version) < 0 || compareVersions(result.host.version, result.min_supported_version) < 0) throw invalid();
  if (result.target.startsWith('windows-')) {
    if (files.length !== 1 || !files[0]!.name.endsWith('.msi')) throw invalid();
  } else if (files.length !== 2 || !files.find(f => f.purposes.includes('installer'))!.name.endsWith('.dmg')
    || !files.find(f => f.purposes.includes('updater'))!.name.endsWith('.app.tar.gz')) throw invalid();
  return result;
}
