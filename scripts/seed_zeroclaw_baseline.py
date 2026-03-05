#!/usr/bin/env python3
"""Seed the ZeroClaw baseline memory.db with 8 synthetic memory rows."""

import sqlite3
import sys

def main():
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <db_path>", file=sys.stderr)
        sys.exit(1)

    db_path = sys.argv[1]
    conn = sqlite3.connect(db_path)

    conn.execute("""
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            key TEXT NOT NULL,
            content TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'core',
            timestamp TEXT NOT NULL,
            embedding BLOB
        )
    """)

    rows = [
        ('550e8400-e29b-41d4-a716-446655440001', 'project_stack',
         'The project uses Next.js 14 with App Router, TypeScript strict mode, '
         'Tailwind CSS for styling, and Prisma ORM with PostgreSQL. Testing uses '
         'Vitest for unit tests and Playwright for E2E.',
         'core', '2026-01-10T09:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440002', 'coding_conventions',
         'Team conventions: use named exports, prefer const over let, always type '
         'function parameters explicitly, use zod for runtime validation at API '
         'boundaries, and write JSDoc comments for public APIs.',
         'core', '2026-01-10T09:30:00Z'),

        ('550e8400-e29b-41d4-a716-446655440003', 'auth_architecture',
         'Authentication uses NextAuth.js v5 with the Credentials provider backed '
         'by bcrypt password hashing. Sessions are JWT-based with a 24-hour expiry. '
         'The middleware protects all /dashboard routes.',
         'core', '2026-01-11T10:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440004', 'assistant_autosave_2026-01-12',
         'Helped debug a hydration mismatch in the Dashboard component. The issue '
         'was a Date.now() call in the render path that differed between server and '
         'client. Fixed by moving it to useEffect.',
         'conversation', '2026-01-12T14:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440005', 'deployment_setup',
         'Deployment is on Vercel with preview deployments for PRs. Environment '
         'variables are managed through Vercel project settings. The production '
         'database is on Neon with connection pooling enabled.',
         'core', '2026-01-13T11:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440006', 'assistant_autosave_2026-01-14',
         'Implemented the user settings page with form validation using '
         'react-hook-form and zod. Added optimistic updates for the profile '
         'picture upload using the useOptimistic hook.',
         'conversation', '2026-01-14T16:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440007', 'api_patterns',
         'API routes follow a consistent pattern: validate input with zod, '
         'authenticate via getServerSession, perform database operation with '
         'Prisma, and return typed responses. Error handling uses a shared '
         'ApiError class.',
         'core', '2026-01-15T09:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440008', 'assistant_autosave_2026-01-15',
         'Pair-programmed on the notification system. Used Server-Sent Events for '
         'real-time updates instead of WebSockets to keep the infrastructure simple. '
         'The EventSource API handles reconnection automatically.',
         'conversation', '2026-01-15T15:00:00Z'),
    ]

    conn.executemany(
        "INSERT INTO memories (id, key, content, category, timestamp) "
        "VALUES (?, ?, ?, ?, ?)",
        rows
    )
    conn.commit()
    conn.close()


if __name__ == "__main__":
    main()
