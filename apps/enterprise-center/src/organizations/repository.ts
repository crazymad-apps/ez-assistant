import type { EntityManager } from 'typeorm';
import type { MemberPage, MemberQuery, Organization, SaveMember } from '../contracts/organizations.js';

export const organizations = {
  async get(manager: EntityManager, id: string): Promise<Organization | undefined> {
    const rows: Organization[] = await manager.query(
      'SELECT id, code, name, enabled FROM public.organizations WHERE id = $1', [id],
    );
    return rows[0];
  },

  list(manager: EntityManager): Promise<Organization[]> {
    return manager.query('SELECT id, code, name, enabled FROM public.organizations ORDER BY code');
  },

  forAccount(manager: EntityManager, userId: number): Promise<(Organization & { member_role: SaveMember['role'] })[]> {
    return manager.query(`SELECT o.id, o.code, o.name, o.enabled, m.role AS member_role
      FROM public.organizations o JOIN public.organization_members m ON m.organization_id = o.id
      WHERE o.enabled AND m.enabled AND m.user_id = $1 ORDER BY o.code`, [userId]);
  },

  async member(manager: EntityManager, organizationId: string, userId: number): Promise<SaveMember | undefined> {
    const rows: SaveMember[] = await manager.query(
      'SELECT role, enabled FROM public.organization_members WHERE organization_id = $1 AND user_id = $2',
      [organizationId, userId],
    );
    return rows[0];
  },

  async members(manager: EntityManager, organizationId: string, query: MemberQuery & { limit: number; offset: number }): Promise<MemberPage> {
    const [result]: Omit<MemberPage, 'limit' | 'offset'>[] = await manager.query(`WITH filtered AS (
      SELECT u.id AS user_id, u.username, u.display_name, u.enabled AS account_enabled, m.role, m.enabled
      FROM public.organization_members m JOIN public.users u ON u.id = m.user_id
      WHERE m.organization_id = $1
      AND ($2::text IS NULL OR strpos(lower(u.username), lower($2)) > 0 OR strpos(lower(u.display_name), lower($2)) > 0)
      AND (NOT $7::boolean OR (m.enabled AND u.enabled AND EXISTS (
        SELECT 1 FROM public.organizations o WHERE o.id = m.organization_id AND o.enabled
      )))
      AND ($3::text IS NULL OR m.role = $3) AND ($4::boolean IS NULL OR m.enabled = $4)
    ), page AS (SELECT * FROM filtered ORDER BY username LIMIT $5 OFFSET $6)
    SELECT (SELECT count(*)::integer FROM filtered) AS total,
      COALESCE((SELECT json_agg(page ORDER BY username) FROM page), '[]'::json) AS items`,
    [organizationId, query.search ?? null, query.role ?? null, query.enabled ?? null, query.limit, query.offset, query.active_only ?? false]);
    return { ...result!, limit: query.limit, offset: query.offset };
  },

  async hasAdmin(manager: EntityManager, organizationId: string, excluding?: number): Promise<boolean> {
    const [row]: { found: boolean }[] = await manager.query(`SELECT EXISTS (
      SELECT 1 FROM public.organization_members m JOIN public.users u ON u.id = m.user_id
      WHERE m.organization_id = $1 AND m.role = 'admin' AND m.enabled AND u.enabled
      AND ($2::integer IS NULL OR m.user_id <> $2)
    ) AS found`, [organizationId, excluding ?? null]);
    return row!.found;
  },

  async leavesOrganizationWithoutAdmin(manager: EntityManager, userId: number): Promise<boolean> {
    const [row]: { found: boolean }[] = await manager.query(`SELECT EXISTS (
      SELECT 1 FROM public.organization_members m JOIN public.organizations o ON o.id = m.organization_id
      WHERE m.user_id = $1 AND o.enabled AND m.enabled AND m.role = 'admin' AND NOT EXISTS (
        SELECT 1 FROM public.organization_members other JOIN public.users u ON u.id = other.user_id
        WHERE other.organization_id = m.organization_id AND other.user_id <> $1
        AND other.role = 'admin' AND other.enabled AND u.enabled
      )
    ) AS found`, [userId]);
    return row!.found;
  },

  async saveMember(manager: EntityManager, organizationId: string, userId: number, input: SaveMember) {
    await manager.query(`INSERT INTO public.organization_members (organization_id, user_id, role, enabled)
      VALUES ($1, $2, $3, $4) ON CONFLICT (organization_id, user_id)
      DO UPDATE SET role = EXCLUDED.role, enabled = EXCLUDED.enabled, updated_at = CURRENT_TIMESTAMP`,
    [organizationId, userId, input.role, input.enabled]);
  },
};
