import { releaseNotes } from '../site-content/markdown.js';
import type { SiteChangelogEntry, SiteDownload } from '../contracts/site-content.js';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import type { DataSource, EntityManager } from 'typeorm';
import type { FastifyRequest } from 'fastify';
import { enterpriseProtocol } from '../protocol.js';
import { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { DesktopReleaseEntity, DesktopReleaseTargetEntity } from './entity.js';
import { DesktopReleaseFiles } from './files.js';
import { compareVersions, desktopTargets, invalid, limitedText, object, parseManifest, target, verifyReleaseSignature, version } from './manifest.js';
import type { ReleaseManifest } from './manifest.js';

export class DesktopReleaseService {
  private publicKey?: string;

  constructor(private readonly identities: IdentityService, private readonly connection: DataSource | undefined,
    readonly files: DesktopReleaseFiles, private readonly origin?: string, private readonly keyFile?: string) {}

  private get source(): DataSource {
    if (!this.connection?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.connection;
  }

  async initialize(): Promise<void> {
    const bundled = new URL('../resources/desktop-update.pub', import.meta.url);
    const key = this.keyFile ?? (existsSync(bundled) ? bundled : undefined);
    if (key) this.publicKey = limitedText(await readFile(key, 'utf8'), 8192);
  }

  onModuleDestroy(): Promise<void> { return this.files.close(); }

  private async detail(manager: EntityManager, releaseVersion: string) {
    const release = await manager.findOneBy(DesktopReleaseEntity, { version: releaseVersion });
    if (!release) throw new IdentityError(404, 'DESKTOP_RELEASE_NOT_FOUND');
    const rows = await manager.findBy(DesktopReleaseTargetEntity, { version: releaseVersion });
    return { version: release.version, notes: release.notes, created_at: release.created_at.toISOString(), updated_at: release.updated_at.toISOString(),
      targets: rows.map(row => ({ manifest_text: row.manifest_text, target: row.target, publication: row.publication, manifest_sha256: row.manifest_sha256,
        first_published_at: row.first_published_at?.toISOString() ?? null, manifest: parseManifest(row.manifest_text) })) };
  }

  async get(releaseVersion: string, token?: string) {
    version(releaseVersion);
    await this.identities.requireAdmin(this.source.manager, token);
    const result = await this.detail(this.source.manager, releaseVersion);
    await this.identities.requireAdmin(this.source.manager, token);
    return result;
  }

  async list(token?: string) {
    await this.identities.requireAdmin(this.source.manager, token);
    const releases = await this.source.manager.find(DesktopReleaseEntity);
    releases.sort((a, b) => compareVersions(b.version, a.version));
    const rows = await this.source.manager.find(DesktopReleaseTargetEntity);
    const result = releases.map(release => ({ version: release.version, notes: release.notes.slice(0, 200),
      targets: rows.filter(row => row.version === release.version).map(row => ({ target: row.target, publication: row.publication })) }));
    await this.identities.requireAdmin(this.source.manager, token);
    return result;
  }

  /** 发布历史只投影现有版本行；单条 SQL 固定正文与首次发布事实，撤回不会抹去日志。 */
  async changelog(): Promise<SiteChangelogEntry[]> {
    const rows: { version: string; notes: string; published_at: Date }[] = await this.source.query(`
      SELECT r.version, r.notes, MIN(t.first_published_at) AS published_at
      FROM desktop_releases r JOIN desktop_release_targets t ON t.version = r.version
      WHERE t.first_published_at IS NOT NULL AND btrim(r.notes) <> ''
      GROUP BY r.version, r.notes`);
    rows.sort((a, b) => compareVersions(b.version, a.version));
    return rows.filter(row => row.notes.trim()).map(row => ({ version: row.version, published_at: row.published_at.toISOString(), body: releaseNotes(row.notes) }));
  }

  /** 首次安装按平台独立选择；每次复核发布状态与实际文件，不缓存最新版本指针。 */
  async downloads(input: unknown): Promise<SiteDownload[]> {
    const query = object(input, ['version']);
    const requested = query.version === undefined ? undefined : version(query.version);
    const rows = await this.source.manager.findBy(DesktopReleaseTargetEntity, { publication: 'published', ...(requested ? { version: requested } : {}) });
    rows.sort((a, b) => compareVersions(b.version, a.version));
    const result: SiteDownload[] = [];
    for (const platform of desktopTargets) {
      for (const row of rows.filter(item => item.target === platform)) {
        try {
          const selected = await this.published(platform, row.version);
          if (!selected.manifest.host.enterprise_protocols.some(value => value >= enterpriseProtocol.minimum && value <= enterpriseProtocol.current)) continue;
          const file = selected.manifest.files.find(item => item.purposes.includes('installer'))!;
          result.push({ target: platform, version: row.version, published_at: selected.row.first_published_at!.toISOString(),
            name: file.name, size: file.size, sha256: file.sha256,
            url: `/api/desktop-updates/${platform}/releases/${row.version}/files/installer` });
          break;
        } catch (error) {
          if (!(error instanceof IdentityError) || !['DESKTOP_RELEASE_NOT_FOUND', 'DESKTOP_RELEASE_FILE_UNAVAILABLE'].includes(error.code)) throw error;
        }
      }
    }
    return result;
  }

  /** 清单/签名必须是最前两个文本字段；平台文件不进入内存，也不在上传期间持有身份事务。 */
  async upload(request: FastifyRequest, token: string | undefined, requestId: string, client: AbortSignal) {
    return this.identities.attempt('desktop_release_uploaded', requestId, () => this.files.run(async signal => {
      if (!this.publicKey) throw new IdentityError(503, 'DESKTOP_RELEASE_KEY_UNAVAILABLE');
      const parts = request.parts({ limits: { fields: 2, files: 2, parts: 4, fieldSize: 96 * 1024, fileSize: 512 * 1024 * 1024, fieldNameSize: 64 } });
      let text = '', signature = '', manifest: ReleaseManifest | undefined;
      let original: DesktopReleaseTargetEntity | null = null;
      const received = new Set<string>();
      const abort = () => request.raw.destroy();
      signal.addEventListener('abort', abort, { once: true });
      try {
        for await (const part of parts) {
          signal.throwIfAborted();
          if (part.type === 'field') {
            if (part.valueTruncated) throw invalid();
            if (!text && part.fieldname === 'manifest') {
              // 浏览器 FormData 会把文本 LF 改为 CRLF；小清单用 base64 传输以保留签名原字节。
              const encoded = limitedText(part.value, 96 * 1024);
              const bytes = Buffer.from(encoded, 'base64');
              if (bytes.length > 64 * 1024 || bytes.toString('base64') !== encoded) throw invalid();
              text = bytes.toString('utf8');
              if (!Buffer.from(text).equals(bytes)) throw invalid();
            }
            else if (text && !signature && part.fieldname === 'signature') {
              signature = limitedText(part.value, 8192);
              verifyReleaseSignature(Buffer.from(text), signature, this.publicKey);
              manifest = parseManifest(text);
              original = await this.source.manager.findOneBy(DesktopReleaseTargetEntity, { version: manifest.version, target: manifest.target });
              if (original?.first_published_at && original.manifest_sha256 !== createHash('sha256').update(text).digest('hex'))
                throw new IdentityError(409, 'DESKTOP_RELEASE_IMMUTABLE');
            } else throw invalid();
          } else {
            if (!manifest || part.fieldname !== 'files' || received.has(part.filename)) { part.file.destroy(); throw invalid(); }
            const expected = manifest.files.find(file => file.name === part.filename);
            if (!expected) { part.file.destroy(); throw invalid(); }
            await this.files.receive(part.file, expected, signal);
            if (part.file.truncated) throw invalid();
            received.add(part.filename);
          }
        }
        if (!manifest || received.size !== manifest.files.length) throw invalid();
        signal.throwIfAborted();
        const accepted = manifest;
        const sha = createHash('sha256').update(text).digest('hex');
        return await this.identities.write(async manager => {
          const actor = await this.identities.requireAdmin(manager, token);
          signal.throwIfAborted();
          const current = await manager.findOneBy(DesktopReleaseTargetEntity, { version: accepted.version, target: accepted.target });
          if (current?.manifest_sha256 === sha) return { version: accepted.version, target: accepted.target, manifest_sha256: sha, result: 'unchanged' as const };
          if (current?.first_published_at) throw new IdentityError(409, 'DESKTOP_RELEASE_IMMUTABLE');
          if ((current?.manifest_sha256 ?? null) !== (original?.manifest_sha256 ?? null)
            || (current?.publication ?? null) !== (original?.publication ?? null)) throw new IdentityError(409, 'DESKTOP_RELEASE_CHANGED');
          if (!await manager.existsBy(DesktopReleaseEntity, { version: accepted.version }))
            await manager.insert(DesktopReleaseEntity, { version: accepted.version, notes: '', created_at: new Date(), updated_at: new Date() });
          await manager.save(DesktopReleaseTargetEntity, { version: accepted.version, target: accepted.target,
            manifest_text: text, manifest_signature: signature, manifest_sha256: sha, publication: 'draft', first_published_at: null });
          const result = current ? 'replaced' as const : 'created' as const;
          await this.identities.audit(manager, 'desktop_release_uploaded', requestId, actor.id, undefined, null,
            { version: accepted.version, target: accepted.target, sha256: sha, result });
          return { version: accepted.version, target: accepted.target, manifest_sha256: sha, result };
        });
      } catch (error) {
        if (error instanceof Error && 'code' in error && ['FST_REQ_FILE_TOO_LARGE', 'FST_PARTS_LIMIT', 'FST_FILES_LIMIT', 'FST_FIELDS_LIMIT'].includes(String(error.code)))
          throw new IdentityError(413, 'DESKTOP_RELEASE_FILE_INVALID');
        throw error;
      } finally { signal.removeEventListener('abort', abort); }
    }, client));
  }

  async notes(releaseVersion: string, input: unknown, token: string | undefined, requestId: string) {
    return this.identities.attempt('desktop_release_notes_changed', requestId, async () => {
      version(releaseVersion);
      const body = object(input, ['notes', 'expected_notes']);
      const notes = limitedText(body.notes, 16 * 1024), expected = limitedText(body.expected_notes, 16 * 1024);
      return this.identities.write(async manager => {
        const actor = await this.identities.requireAdmin(manager, token);
        const row = await manager.findOneBy(DesktopReleaseEntity, { version: releaseVersion });
        if (!row) throw new IdentityError(404, 'DESKTOP_RELEASE_NOT_FOUND');
        if (row.notes !== notes) {
          if (row.notes !== expected) throw new IdentityError(409, 'DESKTOP_RELEASE_CHANGED');
          await manager.update(DesktopReleaseEntity, { version: releaseVersion }, { notes, updated_at: new Date() });
          await this.identities.audit(manager, 'desktop_release_notes_changed', requestId, actor.id, undefined, null, { version: releaseVersion });
        }
        return this.detail(manager, releaseVersion);
      });
    });
  }

  async publish(releaseVersion: string, releaseTarget: string, input: unknown, token: string | undefined, requestId: string) {
    return this.identities.attempt('desktop_release_publication_changed', requestId, async () => {
      version(releaseVersion); target(releaseTarget);
      const body = object(input, ['manifest_sha256', 'expected_publication', 'publication']);
      if (!['draft', 'published', 'withdrawn'].includes(String(body.expected_publication))
        || !['published', 'withdrawn'].includes(String(body.publication))) throw invalid();
      return this.identities.write(async manager => {
        const actor = await this.identities.requireAdmin(manager, token);
        const row = await manager.findOneBy(DesktopReleaseTargetEntity, { version: releaseVersion, target: target(releaseTarget) });
        if (!row) throw new IdentityError(404, 'DESKTOP_RELEASE_NOT_FOUND');
        if (row.manifest_sha256 !== body.manifest_sha256) throw new IdentityError(409, 'DESKTOP_RELEASE_CHANGED');
        if (row.publication !== body.publication) {
          if (row.publication !== body.expected_publication || (row.publication === 'draft' && body.publication === 'withdrawn'))
            throw new IdentityError(409, 'DESKTOP_RELEASE_CHANGED');
          if (body.publication === 'published' && !await this.files.available(parseManifest(row.manifest_text).files))
            throw new IdentityError(503, 'DESKTOP_RELEASE_FILE_UNAVAILABLE');
          row.publication = body.publication as 'published' | 'withdrawn';
          row.first_published_at ??= new Date();
          await manager.save(row);
          await this.identities.audit(manager, 'desktop_release_publication_changed', requestId, actor.id, undefined, null,
            { version: releaseVersion, target: releaseTarget, release_publication: row.publication, sha256: row.manifest_sha256 });
        }
        return this.detail(manager, releaseVersion);
      }, () => { if (body.publication === 'withdrawn') this.files.cancelDownloads(`${releaseVersion}/${releaseTarget}`); });
    });
  }

  /** 公共下载与撤回共用短串行准入；撤回提交后不会新建漏网下载流。 */
  async published(releaseTarget: string, releaseVersion: string, purpose?: string) {
    target(releaseTarget); version(releaseVersion);
    if (purpose !== undefined && !['installer', 'updater'].includes(purpose)) throw invalid();
    let opened: import('node:stream').Readable | undefined;
    try { return await this.identities.write(async manager => {
      const row = await manager.findOneBy(DesktopReleaseTargetEntity, { target: target(releaseTarget), version: releaseVersion, publication: 'published' });
      if (!row) throw new IdentityError(404, 'DESKTOP_RELEASE_NOT_FOUND');
      const manifest = parseManifest(row.manifest_text);
      if (!await this.files.available(manifest.files)) throw new IdentityError(503, 'DESKTOP_RELEASE_FILE_UNAVAILABLE');
      const release = await manager.findOneByOrFail(DesktopReleaseEntity, { version: releaseVersion });
      const file = manifest.files.find(file => file.purposes.includes(purpose as 'installer' | 'updater'));
      const stream = file ? await this.files.download(`${releaseVersion}/${releaseTarget}`, file) : undefined;
      opened = stream;
      return { row, manifest, notes: release.notes, file, stream };
    }); } catch (error) { opened?.destroy(); throw error; }
  }

  private response(candidate: Awaited<ReturnType<DesktopReleaseService['published']>>) {
    if (!this.origin) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    const { row, manifest, notes } = candidate;
    return { version: row.version, notes, pub_date: row.first_published_at!.toISOString(),
      url: `${this.origin}/api/desktop-updates/${row.target}/releases/${row.version}/files/updater`,
      signature: manifest.files.find(file => file.purposes.includes('updater'))!.signature!,
      manifest: row.manifest_text, manifest_signature: row.manifest_signature, manifest_sha256: row.manifest_sha256 };
  }

  async fixed(releaseTarget: string, releaseVersion: string) { return this.response(await this.published(releaseTarget, releaseVersion)); }

  async discover(releaseTarget: string, current: string, input: unknown) {
    target(releaseTarget); version(current);
    const query = object(input, ['host_version', 'host_min_supported_version', 'host_protocols']);
    const remote = Object.keys(query).length > 0;
    let hostVersion = '', hostMin = '', protocols: number[] = [];
    if (remote) {
      hostVersion = version(query.host_version); hostMin = version(query.host_min_supported_version);
      if (typeof query.host_protocols !== 'string' || !/^[1-9]\d{0,4}(,[1-9]\d{0,4}){0,15}$/.test(query.host_protocols)) throw invalid();
      protocols = query.host_protocols.split(',').map(Number);
    }
    const rows = await this.source.manager.findBy(DesktopReleaseTargetEntity, { target: target(releaseTarget), publication: 'published' });
    rows.sort((a, b) => compareVersions(b.version, a.version));
    for (const row of rows) {
      if (compareVersions(row.version, current) <= 0) continue;
      const manifest = parseManifest(row.manifest_text);
      // 企业协议与软件版本分别判定；本机随包更新只使用新 Host 的事实，远端使用保留 Host 的事实。
      if (remote ? (compareVersions(manifest.version, hostMin) < 0 || compareVersions(hostVersion, manifest.min_supported_version) < 0 || !protocols.some(value => value >= enterpriseProtocol.minimum && value <= enterpriseProtocol.current))
        : !manifest.host.enterprise_protocols.some(value => value >= enterpriseProtocol.minimum && value <= enterpriseProtocol.current)) continue;
      try { return await this.fixed(releaseTarget, row.version); }
      catch (error) { if (!(error instanceof IdentityError) || !['DESKTOP_RELEASE_NOT_FOUND', 'DESKTOP_RELEASE_FILE_UNAVAILABLE'].includes(error.code)) throw error; }
    }
    return null;
  }
}
