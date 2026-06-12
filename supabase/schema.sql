-- phase.rs cloud sync schema.
--
-- Run once in the Supabase SQL editor (or via the CLI) for the official
-- deployment's project. Stores one PhaseBackup envelope per user, scoped by
-- Row-Level Security so the browser can read/write its own row directly with
-- the public anon key and never sees anyone else's data.
--
-- Self-hosters who don't run Supabase skip this entirely; the client disables
-- cloud sync when no Supabase build env is set.

create table if not exists public.user_backups (
  user_id    uuid        primary key references auth.users (id) on delete cascade,
  payload    jsonb       not null,
  revision   bigint      not null default 1,
  updated_at timestamptz not null default now()
);

alter table public.user_backups enable row level security;

-- A user may only ever touch their own row.
drop policy if exists own_backup on public.user_backups;
create policy own_backup on public.user_backups
  for all
  using (auth.uid() = user_id)
  with check (auth.uid() = user_id);

-- Lightweight realtime signal. Mirrors (revision, updated_at) from
-- user_backups, maintained atomically by upsert_backup (below). It exists ONLY
-- so peer devices get a tiny CDC event on every push: Supabase Realtime
-- (wal2json/WALRUS) broadcasts the full changed row and ignores publication
-- column lists, so the heavy `payload` jsonb must not live in any
-- realtime-published table. Realtime watches THIS table; user_backups remains
-- the system of record and is removed from the publication below.
create table if not exists public.user_backup_revisions (
  user_id    uuid        primary key references auth.users (id) on delete cascade,
  revision   bigint      not null,
  updated_at timestamptz not null
);

alter table public.user_backup_revisions enable row level security;

-- Realtime postgres_changes delivery is gated by the SELECT RLS policy AND a
-- table-level GRANT SELECT to the token role; the user must be able to read
-- their own signal row to receive its CDC events. (RLS enabled with no policy =
-- deny-all = no events, so both this policy and the grant below are required.)
drop policy if exists own_revision on public.user_backup_revisions;
create policy own_revision on public.user_backup_revisions
  for all
  using (auth.uid() = user_id)
  with check (auth.uid() = user_id);

-- Read-only to clients; the projection is written exclusively by upsert_backup
-- (security definer). No INSERT/UPDATE grant keeps the single write authority.
grant select on public.user_backup_revisions to authenticated;

-- Atomic compare-and-set upsert. Inserts when the caller has no row yet
-- (p_expected_revision is null); otherwise updates only when the stored
-- revision matches what the caller last saw, bumping the revision. A mismatch
-- means another device wrote in between — raise so the client surfaces a
-- conflict (PostgREST maps `raise exception` to SQLSTATE P0001).
--
-- security definer + the explicit auth.uid() write target keep the function
-- safe: it can only ever write the caller's own row, regardless of arguments.
create or replace function public.upsert_backup(
  p_payload jsonb,
  p_expected_revision bigint
)
returns table (revision bigint, updated_at timestamptz)
language plpgsql
security definer
set search_path = public
as $$
declare
  v_uid uuid := auth.uid();
  v_current bigint;
begin
  if v_uid is null then
    raise exception 'not authenticated';
  end if;

  select b.revision into v_current
  from public.user_backups b
  where b.user_id = v_uid;

  if v_current is null then
    -- First write for this account. If another device inserted between the
    -- select above and this insert, the PK raises unique_violation — convert it
    -- to the same conflict signal (P0001) so the client shows the keep-cloud/
    -- keep-local prompt instead of a hard error.
    begin
      return query
      insert into public.user_backups (user_id, payload, revision, updated_at)
      values (v_uid, p_payload, 1, now())
      returning user_backups.revision, user_backups.updated_at;
    exception when unique_violation then
      raise exception 'revision conflict: row created concurrently';
    end;
    -- Mirror the new revision into the realtime signal table. Placed AFTER the
    -- begin/exception block (not inside it) so a projection-side violation is
    -- never mislabeled as the concurrent-insert P0001 above. `return query`
    -- does not exit the function, so this runs after the result set is built.
    -- on conflict do update self-heals if a stale projection row pre-exists.
    insert into public.user_backup_revisions (user_id, revision, updated_at)
    values (v_uid, 1, now())
    on conflict (user_id) do update
      set revision = excluded.revision, updated_at = excluded.updated_at;
  elsif p_expected_revision is distinct from v_current then
    raise exception 'revision conflict: expected %, found %',
      p_expected_revision, v_current;
  else
    return query
    update public.user_backups
    set payload = p_payload,
        revision = v_current + 1,
        updated_at = now()
    where user_id = v_uid
    returning user_backups.revision, user_backups.updated_at;
    -- Mirror the bumped revision (v_current + 1, matching the row above) into
    -- the realtime signal table, same transaction → the projection can never
    -- diverge from the source of record. The two now() calls differ by
    -- microseconds; updated_at is display-only (clients key change-detection on
    -- revision), so the skew is acceptable.
    insert into public.user_backup_revisions (user_id, revision, updated_at)
    values (v_uid, v_current + 1, now())
    on conflict (user_id) do update
      set revision = excluded.revision, updated_at = excluded.updated_at;
  end if;
end;
$$;

-- Explicit least-privilege grants so this schema is self-contained even with
-- Supabase's "Automatically expose new tables" DISABLED (the recommended,
-- manual-control setting). Reads go straight to the table (RLS gates them to the
-- caller's own row); writes go only through the security-definer RPC, so the
-- authenticated role needs no direct INSERT/UPDATE on the table.
grant select on public.user_backups to authenticated;
grant execute on function public.upsert_backup(jsonb, bigint) to authenticated;

-- Backfill the realtime signal table for accounts that synced before it
-- existed, so they keep getting peer-device CDC notifications. Idempotent.
-- Runs AFTER `create or replace function upsert_backup` above: once the
-- mirror-writing function is in place, the only window where a push could land
-- a user_backups row without a projection row is inside this single SQL-editor
-- apply, and any such row self-heals on that account's next push (the
-- visibility/pullMeta fallback keeps sync correct meanwhile).
-- `do update` (not `do nothing`) makes re-runs self-healing: the projection is
-- always derived from user_backups (same transaction in the RPC), so it can
-- only ever equal or lag the source — overwriting with the source's current
-- (revision, updated_at) repairs any stale row and is a no-op when already equal.
insert into public.user_backup_revisions (user_id, revision, updated_at)
select user_id, revision, updated_at from public.user_backups
on conflict (user_id) do update
  set revision = excluded.revision, updated_at = excluded.updated_at;

-- Realtime: peer devices receive Postgres CDC notifications via the lightweight
-- user_backup_revisions projection, NOT user_backups — so the heavy `payload`
-- jsonb is never streamed over the WebSocket. The client subscribes via
-- `supabase.channel(...).on('postgres_changes', ...)` on user_backup_revisions;
-- without this membership, `subscribe()` returns silently but no events fire.
-- Drop + add live in one do-block so they roll back as a unit on error (a failed
-- add can't strand user_backups half-removed from realtime); idempotent guards
-- make re-running the schema safe.
do $$
begin
  -- The payload-bearing table must NOT broadcast: remove it from realtime.
  if exists (
    select 1 from pg_publication_tables
    where pubname = 'supabase_realtime'
      and schemaname = 'public' and tablename = 'user_backups'
  ) then
    alter publication supabase_realtime drop table public.user_backups;
  end if;
  -- The lightweight projection IS the realtime signal source.
  if not exists (
    select 1 from pg_publication_tables
    where pubname = 'supabase_realtime'
      and schemaname = 'public' and tablename = 'user_backup_revisions'
  ) then
    alter publication supabase_realtime add table public.user_backup_revisions;
  end if;
end $$;
