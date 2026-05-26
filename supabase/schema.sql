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

-- Realtime: enroll user_backups in the supabase_realtime publication so peer
-- devices receive Postgres CDC notifications when this user's row updates.
-- The client subscribes via `supabase.channel(...).on('postgres_changes', ...)`;
-- without this membership, `subscribe()` returns silently but no events ever
-- fire. Guard with a do-block so re-running the schema is idempotent.
do $$
begin
  if not exists (
    select 1
    from pg_publication_tables
    where pubname = 'supabase_realtime'
      and schemaname = 'public'
      and tablename = 'user_backups'
  ) then
    alter publication supabase_realtime add table public.user_backups;
  end if;
end $$;
