import type { Readable } from 'node:stream';
import type { DataSource, EntityManager } from 'typeorm';
import type { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import type { PackageCurrent, PackageUploadResult, PluginDetail, PluginRecord, PluginResolution } from '../contracts/plugins.js';
import { PluginPackageEntity, PluginEntity } from './entity.js';
import { choosePackage, compareVersions, descriptionInput, packageSummary, pluginEtag,
  publicationInput, requireEtag, resolveInput, validPluginName, versionParts } from './catalog.js';
import { PluginFiles } from './files.js';
import type { PreparedPackage } from './files.js';
import { PluginError } from './errors.js';
import { pluginName } from './manifest.js';

/** 文件先独立发布，数据库再持有唯一当前引用；孤儿文件不反向生成插件版本。 */
export class PluginService {
  constructor(
    private readonly identities: IdentityService,
    private readonly source: DataSource | undefined,
    readonly files: PluginFiles,
  ) {}

  onModuleDestroy(): Promise<void> { return this.files.close(); }

  private get manager(): EntityManager {
    if (!this.source?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.source.manager;
  }

  private async rows(manager: EntityManager, name: string): Promise<{
    plugin: PluginEntity; packages: PluginPackageEntity[];
  }> {
    const plugin = await manager.getRepository(PluginEntity).findOneBy({ name });
    if (!plugin) throw new PluginError(404, 'PLUGIN_NOT_FOUND');
    const packages = await manager.getRepository(PluginPackageEntity).findBy({ plugin_name: name });
    return { plugin, packages };
  }

  private record(plugin: PluginEntity, packages: PluginPackageEntity[]): PluginRecord {
    return {
      name: plugin.name, display_name: plugin.display_name, description: plugin.description,
      publication: plugin.publication,
      versions: [...packages].sort((a, b) => compareVersions(b.version, a.version)).map(packageSummary),
    };
  }

  private async detail(manager: EntityManager, name: string): Promise<PluginDetail> {
    const { plugin, packages } = await this.rows(manager, name);
    // 只取依赖 Agent 名称，不把其完整提示词带入插件管理响应。
    const dependents = await manager.query<{ name: string }[]>(`SELECT name FROM public.custom_agents
      WHERE EXISTS (SELECT 1 FROM jsonb_array_elements(skills) AS ref(value) WHERE ref.value->>'plugin'=$1)
         OR EXISTS (SELECT 1 FROM jsonb_array_elements(mcp) AS ref(value) WHERE ref.value->>'plugin'=$1)
      ORDER BY name`, [name]);
    // 详情补全已存清单的配置/外部软件要求；运行时目录仍只返回选包所需摘要。
    const versions = [...packages].sort((a, b) => compareVersions(b.version, a.version)).map(row => ({
      ...packageSummary(row), updated_at: row.updated_at.toISOString(),
      configuration: row.manifest.configuration, external_software: row.manifest.external_software,
    }));
    return { ...this.record(plugin, packages), versions, dependent_agents: dependents.map(item => item.name) };
  }

  /** 管理目录包含未上架与已废弃项；运行时目录只暴露在架项。 */
  async list(token: string | undefined, runtime = false): Promise<PluginRecord[]> {
    const manager = this.manager;
    if (runtime) await this.identities.requireUser(manager, token);
    else await this.identities.requireAdmin(manager, token);
    const plugins = await manager.getRepository(PluginEntity).find({ order: { name: 'ASC' } });
    const selected = runtime ? plugins.filter(row => row.publication === 'listed') : plugins;
    const packages = await manager.getRepository(PluginPackageEntity).find();
    return selected.map(row => this.record(row, packages.filter(item => item.plugin_name === row.name)));
  }

  async get(name: string, token: string | undefined, runtime = false): Promise<{
    body: PluginRecord | PluginDetail; etag?: string;
  }> {
    validPluginName(name);
    if (runtime) {
      const manager = this.manager;
      await this.identities.requireUser(manager, token);
      const { plugin, packages } = await this.rows(manager, name);
      return { body: this.record(plugin, plugin.publication === 'listed' ? packages : []) };
    }
    // 详情与 ETag 必须取自同一次串行读取，不能让并发说明修改拼出旧正文加新 ETag。
    return this.identities.write(async manager => {
      await this.identities.requireAdmin(manager, token);
      const { plugin } = await this.rows(manager, name);
      return { body: await this.detail(manager, name), etag: pluginEtag(plugin) };
    });
  }

  /** 条件写只针对可编辑元数据；包上传不改变 ETag。 */
  async edit(name: string, token: string | undefined, value: unknown, ifMatch: string | undefined,
    requestId: string): Promise<{ body: PluginDetail; etag: string }> {
    validPluginName(name);
    const input = descriptionInput(value);
    return this.identities.write(async manager => {
      const actor = await this.identities.requireAdmin(manager, token);
      const { plugin } = await this.rows(manager, name);
      requireEtag(ifMatch, pluginEtag(plugin));
      if (plugin.display_name !== input.display_name || plugin.description !== input.description) {
        plugin.display_name = input.display_name;
        plugin.description = input.description;
        plugin.updated_at = new Date();
        await manager.getRepository(PluginEntity).save(plugin);
        await this.identities.audit(manager, 'plugin_updated', requestId, actor.id, undefined, null, { name });
      }
      return { body: await this.detail(manager, name), etag: pluginEtag(plugin) };
    });
  }

  /** 下架在事务提交后取消在途下载；重复目标状态不重写或审计。 */
  async setPublication(name: string, token: string | undefined, value: unknown,
    ifMatch: string | undefined, requestId: string): Promise<{ body: PluginDetail; etag: string }> {
    validPluginName(name);
    const listed = publicationInput(value);
    return this.identities.write(async manager => {
      const actor = await this.identities.requireAdmin(manager, token);
      const { plugin, packages } = await this.rows(manager, name);
      requireEtag(ifMatch, pluginEtag(plugin));
      const wasListed = plugin.publication === 'listed';
      const target = listed ? 'listed' : plugin.publication === 'listed' ? 'deprecated' : plugin.publication;
      if (target !== plugin.publication) {
        if (listed && !(await Promise.all(packages.map(row =>
          this.files.check(row.sha256, row.archive_format, Number(row.size_bytes))))).some(Boolean))
          throw new PluginError(503, 'PACKAGE_UNAVAILABLE');
        plugin.publication = target;
        plugin.updated_at = new Date();
        await manager.getRepository(PluginEntity).save(plugin);
        await this.identities.audit(manager, 'plugin_publication_changed', requestId, actor.id,
          undefined, null, { name, publication: target });
      }
      return { body: await this.detail(manager, name), etag: pluginEtag(plugin),
        cancel: wasListed && target === 'deprecated' };
    }, result => { if (result.cancel) this.files.cancelDownloads(name); }).then(
      ({ body, etag }) => ({ body, etag }));
  }

  async resolve(name: string, token: string | undefined, value: unknown): Promise<PluginResolution> {
    validPluginName(name);
    const input = resolveInput(value);
    const manager = this.manager;
    await this.identities.requireUser(manager, token);
    const { plugin, packages } = await this.rows(manager, name);
    this.requireListed(plugin);
    const selected = await choosePackage(packages, input, row =>
      this.files.check(row.sha256, row.archive_format, Number(row.size_bytes)));
    return { package: { name, ...packageSummary(selected) }, update_available: Boolean(input.installed_version &&
      compareVersions(selected.version, input.installed_version) > 0) };
  }

  private requireListed(plugin: PluginEntity): void {
    if (plugin.publication === 'unpublished') throw new PluginError(409, 'PLUGIN_UNPUBLISHED');
    if (plugin.publication === 'deprecated') throw new PluginError(410, 'PLUGIN_DEPRECATED');
  }

  /** 短串行段固定当前 SHA 与文件句柄；HTTP 传输在事务提交后进行。 */
  async download(name: string, version: string, token: string | undefined,
    ifMatch: string | undefined): Promise<{ stream: Readable; package: PluginPackageEntity }> {
    validPluginName(name);
    versionParts(version);
    let admitted: Readable | undefined;
    try {
      return await this.identities.write(async manager => {
        await this.identities.requireUser(manager, token);
        const { plugin } = await this.rows(manager, name);
        this.requireListed(plugin);
        const row = await manager.getRepository(PluginPackageEntity).findOneBy({ plugin_name: name, version });
        if (!row) throw new IdentityError(404, 'PACKAGE_NOT_FOUND');
        requireEtag(ifMatch, `"${row.sha256}"`);
        admitted = await this.files.openDownload(name, row.sha256, row.archive_format, Number(row.size_bytes));
        return { stream: admitted, package: row };
      });
    } catch (error) {
      admitted?.destroy();
      if (error instanceof PluginError && error.code === 'CONFIG_CHANGED')
        throw new PluginError(412, 'PACKAGE_CHANGED');
      throw error;
    }
  }

  /** 长传输和归档扫描在事务外；短提交段内重新鉴权，包记录与成功审计一同提交。 */
  async upload(body: Readable, format: PreparedPackage['format'], token: string | undefined,
    requestId: string, actorId: number, clientSignal?: AbortSignal): Promise<PackageUploadResult> {
    try {
      return await this.files.run(async signal => {
      const prepared = await this.files.receive(body, format, signal);
      if (signal.aborted) throw signal.reason;
      await this.files.publish(prepared);
      if (signal.aborted) throw signal.reason;
      try {
        return await this.identities.write(async manager => {
          if (signal.aborted) throw signal.reason;
          const actor = await this.identities.requireAdmin(manager, token);
          if (signal.aborted) throw signal.reason;
          const pluginRepo = manager.getRepository(PluginEntity);
          const packageRepo = manager.getRepository(PluginPackageEntity);
          let plugin = await pluginRepo.findOneBy({ name: prepared.manifest.name });
          if (!plugin) {
            plugin = pluginRepo.create({ name: prepared.manifest.name,
              display_name: prepared.manifest.display_name, description: prepared.manifest.description,
              publication: 'unpublished' });
            await pluginRepo.save(plugin);
          }
          // 身份 write 串行化提交段；并发上传只按进入此段后的实际提交顺序切换当前 SHA。
          const existing = await packageRepo.findOneBy({ plugin_name: prepared.manifest.name,
            version: prepared.manifest.version });
          const result = existing ? existing.sha256 === prepared.sha256 ? 'unchanged' : 'replaced' : 'created';
          // 同 SHA 不更新当前行；异 SHA 只替换当前引用，旧内容寻址文件继续保留。
          if (result !== 'unchanged') {
            await packageRepo.save(packageRepo.create({
              plugin_name: prepared.manifest.name, version: prepared.manifest.version,
              manifest: prepared.manifest, archive_format: prepared.format, sha256: prepared.sha256,
              size_bytes: String(prepared.size), created_at: existing?.created_at ?? new Date(),
              updated_at: new Date(),
            }));
          }
          if (signal.aborted) throw signal.reason;
          await this.identities.audit(manager, 'plugin_package_uploaded', requestId, actor.id, undefined, null, {
            name: prepared.manifest.name, version: prepared.manifest.version,
            platforms: prepared.manifest.platforms, result, sha256: prepared.sha256,
            previous_sha256: result === 'replaced' ? existing!.sha256 : undefined,
          });
          if (signal.aborted) throw signal.reason;
          return {
            name: prepared.manifest.name, version: prepared.manifest.version,
            platforms: prepared.manifest.platforms, archive_format: prepared.format,
            sha256: prepared.sha256, size_bytes: prepared.size, result,
          };
        });
      } catch (error) {
        // 文件已完整发布但事务结果可能未知；保留内容寻址文件，不自动重试或删除。
        if (!(error instanceof IdentityError || error instanceof PluginError))
          throw new PluginError(503, 'PACKAGE_RESULT_UNKNOWN');
        throw error;
      }
      }, clientSignal);
    } catch (error) {
      if (error instanceof PluginError && error.code === 'PACKAGE_BUSY') body.destroy();
      if (error instanceof PluginError && error.status < 500 && this.source?.isInitialized)
        await this.identities.audit(this.source.manager, 'plugin_package_uploaded', requestId,
          actorId, error.code, null);
      throw error;
    }
  }

  /** 供管理员在上传回包丢失后核对当前 SHA；这里只读元数据，不开放包下载。 */
  async current(name: string, version: string, token?: string): Promise<PackageCurrent> {
    if (!pluginName.test(name) || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version))
      throw new IdentityError(400, 'INVALID_REQUEST');
    if (!this.source?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    await this.identities.requireAdmin(this.source.manager, token);
    const row = await this.source.getRepository(PluginPackageEntity).findOneBy({ plugin_name: name, version });
    if (!row) throw new IdentityError(404, 'PACKAGE_NOT_FOUND');
    if (!(await this.files.check(row.sha256, row.archive_format, Number(row.size_bytes))))
      throw new PluginError(503, 'PACKAGE_UNAVAILABLE');
    return { name, version, platforms: row.manifest.platforms, archive_format: row.archive_format,
      sha256: row.sha256, size_bytes: Number(row.size_bytes) };
  }
}
