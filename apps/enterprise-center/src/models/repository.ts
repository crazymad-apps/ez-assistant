import type { EntityManager } from 'typeorm';
import { CenterIdentityEntity } from '../identity/entity.js';
import { randomUUID } from 'node:crypto';
import type { CatalogModel, ModelSelection, ProviderConnection, ModelParameters, ModelPolicy } from '../contracts/models.js';
import { ProviderEntity, FixedConfigEntity, ModelSettingsEntity, OrganizationModelEntity } from './entity.js';

/** 使用调用者事务的 Manager；不拥有连接池，秘密仅在明确的内部读取入口出现。 */
export const models = {
  provider(manager: EntityManager, organizationId: string, id: string) {
    return manager
      .getRepository(ProviderEntity)
      .createQueryBuilder('p')
      .addSelect('p.api_key')
      .addSelect('p.server_history_key')
      .where('p.provider_instance_id = :id AND p.organization_id = :organizationId', { id, organizationId })
      .getOne();
  },

  insert(manager: EntityManager, organizationId: string, id: string, connection: ProviderConnection, api_key: string) {
    return manager.getRepository(ProviderEntity).insert({
      provider_instance_id: id,
      organization_id: organizationId,
      connection,
      api_key,
      server_history_key: randomUUID(),
      model_catalog: null,
      catalog_refreshed_at: null,
      catalog_connection_changed: false,
    });
  },

  update(manager: EntityManager, row: ProviderEntity) {
    return manager.getRepository(ProviderEntity).update({ provider_instance_id: row.provider_instance_id }, row);
  },

  renewHistory(manager: EntityManager, provider: ProviderEntity) {
    return manager.getRepository(ProviderEntity).update(
      { provider_instance_id: provider.provider_instance_id },
      { server_history_key: randomUUID() },
    );
  },

  remove(manager: EntityManager, id: string) {
    return manager.getRepository(ProviderEntity).delete({ provider_instance_id: id });
  },

  catalog(manager: EntityManager, id: string, catalog: CatalogModel[]) {
    return manager
      .getRepository(ProviderEntity)
      .update(
        { provider_instance_id: id },
        { model_catalog: catalog, catalog_refreshed_at: new Date(), catalog_connection_changed: false },
      );
  },

  fixed(manager: EntityManager, selection: ModelSelection) {
    return manager.getRepository(FixedConfigEntity).findOneBy(selection);
  },

  async fixedPage(manager: EntityManager, id: string, limit: number, offset: number) {
    const [items, total] = await manager
      .getRepository(FixedConfigEntity)
      .findAndCount({ where: { provider_instance_id: id }, order: { model_id: 'ASC' }, take: limit, skip: offset });
    return { items, total, limit, offset };
  },

  fixedCount(manager: EntityManager, id: string) {
    return manager.getRepository(FixedConfigEntity).countBy({ provider_instance_id: id });
  },

  saveFixed(
    manager: EntityManager,
    selection: ModelSelection,
    origin: 'online' | 'manual',
    parameters: ModelParameters,
  ) {
    return manager.getRepository(FixedConfigEntity).save({ ...selection, origin, parameters, updated_at: new Date() });
  },

  removeFixed(manager: EntityManager, selection: ModelSelection) {
    return manager.getRepository(FixedConfigEntity).delete(selection);
  },

  async policy(manager: EntityManager, organizationId: string): Promise<ModelPolicy> {
    const row = await manager.getRepository(ModelSettingsEntity).findOneBy({ organization_id: organizationId });
    const entries = await manager.getRepository(OrganizationModelEntity).find({
      where: { organization_id: organizationId },
      order: { provider_instance_id: 'ASC', model_id: 'ASC' },
    });
    return {
      default_model: row?.provider_instance_id != null && row.model_id != null
        ? { provider_instance_id: row.provider_instance_id, model_id: row.model_id }
        : null,
      allow_member_selection: row?.allow_member_selection ?? false,
      revision: row?.revision ?? null,
      allowed_models: entries.map(({ provider_instance_id, model_id }) => ({ provider_instance_id, model_id })),
    };
  },

  async publicOrigin(manager: EntityManager): Promise<string | null> {
    return (await manager.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true })).public_origin;
  },

  publicOriginSave(manager: EntityManager, public_origin: string | null) {
    return manager.getRepository(CenterIdentityEntity).update({ singleton: true }, { public_origin });
  },

  // 调用方持有 IdentityService 的写事务。关联替换和默认引用必须同时提交。
  async savePolicy(manager: EntityManager, organizationId: string, policy: ModelPolicy) {
    const entries = manager.getRepository(OrganizationModelEntity);
    await entries.delete({ organization_id: organizationId });
    if (policy.allowed_models.length) {
      await entries.insert(policy.allowed_models.map((entry) => ({ ...entry, organization_id: organizationId })));
    }
    await manager.getRepository(ModelSettingsEntity).upsert({
      organization_id: organizationId,
      provider_instance_id: policy.default_model?.provider_instance_id ?? null,
      model_id: policy.default_model?.model_id ?? null,
      allow_member_selection: policy.allow_member_selection,
      revision: policy.revision,
    }, ['organization_id']);
  },
};
