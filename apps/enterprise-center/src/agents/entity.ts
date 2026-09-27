import { Column, Entity, PrimaryColumn } from 'typeorm';

/** 中心 Agent 配置以名称为身份；M0 仅注册持久化映射，管理入口留到 M2。 */
@Entity({ schema: 'public', name: 'custom_agents', synchronize: false })
export class CustomAgentEntity {
  @PrimaryColumn('text') name!: string;
  @Column('text') description!: string;
  @Column('text') prompt!: string;
  @Column('jsonb') skills!: { name: string; plugin?: string }[];
  @Column('jsonb') mcp!: { name: string; plugin?: string }[];
  @Column('timestamptz') created_at!: Date;
  @Column('timestamptz') updated_at!: Date;
}
