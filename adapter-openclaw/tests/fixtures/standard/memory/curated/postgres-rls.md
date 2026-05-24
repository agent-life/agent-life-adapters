# Postgres Row-Level Security

## What it is

RLS restricts which rows a role can see or modify, enforced by the database.

## Enabling it

Enable per-table and add a policy:

```sql
ALTER TABLE memories ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memories
  USING (tenant_id = current_setting('app.tenant')::uuid);
```

## Gotcha

Policies are only enforced when the setting is applied inside the same
transaction. Setting it outside the transaction silently returns zero rows.
