#!/usr/bin/env python3
"""Apply mutation rounds to the ZeroClaw memory.db.

Usage: mutate_zeroclaw.py <db_path> <round>

Round 1: +3 new rows, 1 updated row
Round 2: +2 new rows, 1 deleted row, 1 updated row
Round 3: +10 new rows (bulk)
"""

import sqlite3
import sys


def apply_round_1(conn):
    new_rows = [
        ('550e8400-e29b-41d4-a716-446655440009', 'testing_strategy',
         'Unit tests go in __tests__ directories co-located with source. '
         'Integration tests for API routes use a test database with Prisma '
         'migrations applied in beforeAll. Playwright E2E tests run against '
         'the preview deployment.',
         'core', '2026-01-16T10:00:00Z'),

        ('550e8400-e29b-41d4-a716-44665544000a', 'assistant_autosave_2026-01-16',
         'Refactored the data fetching layer to use React Server Components with '
         'streaming. Replaced client-side useEffect+fetch with async server '
         'components and Suspense boundaries. Page load time improved by 40%.',
         'conversation', '2026-01-16T14:00:00Z'),

        ('550e8400-e29b-41d4-a716-44665544000b', 'performance_notes',
         'Key performance findings: images should use next/image with blur '
         'placeholders, dynamic imports for heavy components (charts, code '
         'editors), and React.lazy for route-level code splitting.',
         'core', '2026-01-16T16:00:00Z'),
    ]
    conn.executemany(
        "INSERT OR REPLACE INTO memories (id, key, content, category, timestamp) "
        "VALUES (?, ?, ?, ?, ?)",
        new_rows
    )

    conn.execute(
        "UPDATE memories SET content = "
        "'The project uses Next.js 14 with App Router, TypeScript strict mode, "
        "Tailwind CSS for styling, and Prisma ORM with PostgreSQL. Testing uses "
        "Vitest for unit tests and Playwright for E2E. Recently migrated data "
        "fetching to React Server Components with streaming for improved "
        "performance.' "
        "WHERE id = '550e8400-e29b-41d4-a716-446655440001'"
    )


def apply_round_2(conn):
    new_rows = [
        ('550e8400-e29b-41d4-a716-44665544000c', 'assistant_autosave_2026-01-17',
         'Built a custom hook useDebounceValue for the search input. Debounces '
         'by 300ms and cancels pending requests using AbortController. Also '
         'added a search results cache using React context.',
         'conversation', '2026-01-17T11:00:00Z'),

        ('550e8400-e29b-41d4-a716-44665544000d', 'error_handling',
         'Error boundaries: global boundary at app layout level catches render '
         'errors, toast notifications for async errors, and Sentry integration '
         'for error reporting. API errors return structured { error, message, '
         'statusCode } responses.',
         'core', '2026-01-17T14:00:00Z'),
    ]
    conn.executemany(
        "INSERT OR REPLACE INTO memories (id, key, content, category, timestamp) "
        "VALUES (?, ?, ?, ?, ?)",
        new_rows
    )

    conn.execute(
        "DELETE FROM memories "
        "WHERE id = '550e8400-e29b-41d4-a716-446655440004'"
    )

    conn.execute(
        "UPDATE memories SET content = "
        "'Authentication uses NextAuth.js v5 with both Credentials and Google "
        "OAuth providers. Sessions are JWT-based with a 24-hour expiry. The "
        "middleware protects all /dashboard routes. Added CSRF protection and "
        "rate limiting on the login endpoint.' "
        "WHERE id = '550e8400-e29b-41d4-a716-446655440003'"
    )


def apply_round_3(conn):
    rows = [
        ('550e8400-e29b-41d4-a716-446655440101', 'assistant_autosave_2026-01-18a',
         'Implemented infinite scroll for the activity feed using '
         'IntersectionObserver. Loads 20 items per page with a skeleton '
         'loading state.',
         'conversation', '2026-01-18T09:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440102', 'assistant_autosave_2026-01-18b',
         'Added dark mode support using CSS custom properties and a '
         'ThemeProvider context. Persists user preference in localStorage.',
         'conversation', '2026-01-18T14:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440103', 'assistant_autosave_2026-01-19a',
         'Built the file upload component with drag-and-drop using '
         'react-dropzone. Validates file type and size client-side before '
         'uploading to a presigned S3 URL.',
         'conversation', '2026-01-19T10:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440104', 'assistant_autosave_2026-01-19b',
         'Optimized the database queries on the dashboard page. Added compound '
         'indexes on (user_id, created_at) and switched to cursor-based '
         'pagination.',
         'conversation', '2026-01-19T15:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440105', 'state_management',
         'State management: server state via React Server Components and fetch, '
         'client state via React context for UI-only concerns (theme, sidebar '
         'state, modals). No Redux or Zustand needed.',
         'core', '2026-01-19T16:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440106', 'assistant_autosave_2026-01-20',
         'Migrated the email sending from nodemailer to Resend. Simpler API, '
         'better deliverability tracking, and built-in React Email support '
         'for templates.',
         'conversation', '2026-01-20T11:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440107', 'assistant_autosave_2026-01-21',
         'Set up Playwright E2E tests for the critical user flows: signup, '
         'login, create project, invite team member, and billing. Tests run '
         'against Vercel preview deployments in CI.',
         'conversation', '2026-01-21T10:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440108', 'caching_strategy',
         'Caching layers: Next.js data cache for fetch requests with '
         'revalidation tags, React cache() for request deduplication, and '
         'unstable_cache for expensive computations. Redis planned for '
         'session-level caching.',
         'core', '2026-01-21T14:00:00Z'),

        ('550e8400-e29b-41d4-a716-446655440109', 'assistant_autosave_2026-01-22',
         'Added a command palette (Cmd+K) using cmdk library. Indexes all '
         'navigation routes, recent items, and common actions. Search is '
         'fuzzy-matched client-side.',
         'conversation', '2026-01-22T09:00:00Z'),

        ('550e8400-e29b-41d4-a716-44665544010a', 'accessibility_notes',
         'Accessibility audit findings: all interactive elements need visible '
         'focus indicators, images need alt text, form errors need '
         'aria-describedby linking, and the modal needs focus trapping with '
         'inert on background.',
         'core', '2026-01-22T15:00:00Z'),
    ]
    conn.executemany(
        "INSERT OR REPLACE INTO memories (id, key, content, category, timestamp) "
        "VALUES (?, ?, ?, ?, ?)",
        rows
    )


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <db_path> <round>", file=sys.stderr)
        sys.exit(1)

    db_path = sys.argv[1]
    round_num = int(sys.argv[2])

    conn = sqlite3.connect(db_path)

    if round_num == 1:
        apply_round_1(conn)
    elif round_num == 2:
        apply_round_2(conn)
    elif round_num == 3:
        apply_round_3(conn)
    else:
        print(f"Unknown round: {round_num} (expected 1, 2, or 3)", file=sys.stderr)
        sys.exit(1)

    conn.commit()
    conn.close()


if __name__ == "__main__":
    main()
