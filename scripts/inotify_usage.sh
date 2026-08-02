#!/usr/bin/env bash

# Show the processes consuming the most Linux inotify watches. Run as root so
# fd/fdinfo entries belonging to every host user and container are readable.

top_n="${1:-30}"
if [[ ! "$top_n" =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: sudo $0 [top-process-count]" >&2
  exit 2
fi

if (( EUID != 0 )); then
  echo "error: run this script with sudo to inspect all processes" >&2
  echo "usage: sudo $0 [top-process-count]" >&2
  exit 1
fi

echo "inotify limits:"
printf "  max_user_watches:   "
sed -n '1p' /proc/sys/fs/inotify/max_user_watches
printf "  max_user_instances: "
sed -n '1p' /proc/sys/fs/inotify/max_user_instances
printf "  max_queued_events:  "
sed -n '1p' /proc/sys/fs/inotify/max_queued_events
echo
printf "%8s %6s %10s %10s  %s\n" "PID" "UID" "INSTANCES" "WATCHES" "COMMAND"

shopt -s nullglob
for proc_dir in /proc/[0-9]*; do
  [[ -r "$proc_dir/status" ]] || continue

  uid=$(awk '/^Uid:/ {print $2}' "$proc_dir/status" 2>/dev/null)
  uid=${uid:-?}
  instances=0
  watches=0

  for fd in "$proc_dir"/fd/*; do
    target=$(readlink "$fd" 2>/dev/null) || continue
    [[ "$target" == "anon_inode:inotify" ]] || continue

    instances=$((instances + 1))
    fdinfo="$proc_dir/fdinfo/${fd##*/}"
    count=$(awk '
      BEGIN { n=0 }
      /^inotify wd:/ { n++ }
      END { print n }
    ' "$fdinfo" 2>/dev/null)
    case "$count" in
      ''|*[!0-9]*) count=0 ;;
    esac
    watches=$((watches + count))
  done

  if (( watches > 0 || instances > 0 )); then
    command=$(tr '\0' ' ' < "$proc_dir/cmdline" 2>/dev/null)
    if [[ -z "$command" ]]; then
      comm=$(tr -d '\n' < "$proc_dir/comm" 2>/dev/null)
      command="[${comm:-unknown}]"
    fi
    printf "%8s %6s %10s %10s  %.120s\n" \
      "${proc_dir##*/}" "$uid" "$instances" "$watches" "$command"
  fi
done | sort -k4,4nr | head -n "$top_n"
