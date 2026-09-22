import type { EntityManager } from 'typeorm';
import type { CatalogModel, ModelSelection, ProviderConnection, ModelParameters } from '../contracts/models.js';
import { ProviderEntity, FixedConfigEntity, ModelSettingsEntity } from './entity.js';

/** 使用调用者事务的 Manager；不拥有连接池，秘密仅在明确的内部读取入口出现。 */
export const models = {
  list(manager: EntityManager) {
    return manager.getRepository(ProviderEntity).find({ order: { provider_instance_id: 'ASC' } });
  },

  provider(manager: EntityManager, id: string) {
    return manager
      .getRepository(ProviderEntity)
      .createQueryBuilder('p')
      .addSelect('p.api_key')
      .where('p.provider_instance_id = :id', { id })
      .getOne();
  },

  insert(manager: EntityManager, id: string, connection: ProviderConnection, api_key: string) {
    return manager.getRepository(ProviderEntity).insert({
      provider_instance_id: id,
      connection,
      api_key,
      model_catalog: null,
      catalog_refreshed_at: null,
      catalog_connection_changed: false,
    });
  },

  update(manager: EntityManager, row: ProviderEntity) {
    return manager.getRepository(ProviderEntity).update({ provider_instance_id: row.provider_instance_id }, row);
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

  async settings(manager: EntityManager): Promise<ModelSelection | null> {
    const row = await manager.getRepository(ModelSettingsEntity).findOneByOrFail({ singleton: true });
    return row.provider_instance_id !== null && row.model_id !== null
      ? { provider_instance_id: row.provider_instance_id, model_id: row.model_id }
      : null;
  },

  settingsSave(manager: EntityManager, selection: ModelSelection | null) {
    return manager
      .getRepository(ModelSettingsEntity)
      .update(
        { singleton: true },
        { provider_instance_id: selection?.provider_instance_id ?? null, model_id: selection?.model_id ?? null },
      );
  },
};
