# Morning Standup Procedure

A repeatable routine for the daily standup summary. Run every weekday at 09:00.

## Trigger

Fires from cron at 09:00 local time. If the 09:00 run is missed, run manually.

## Steps

1. Pull overnight alerts from the monitoring channel.
2. Summarize blockers, in-progress work, and wins.
3. Post the summary to the team channel.
4. Update `standup-log.json` with the run timestamp.

## Summary template

Fill in this template before posting:

```
## Standup YYYY-MM-DD
- Alerts: <count>
- Blockers: <list>
- In progress: <list>
- Wins: <list>
```

## Failure handling

If posting fails, retry once after 60 seconds. If it fails again, log to
`standup-errors.log` and page the on-call human.

## Notes

Keep the whole thing under five minutes. Brevity beats completeness.
