# Scheduler boundary design

## Goal

Ensure cron schedules fire at their intended wall-clock occurrence regardless
of when the service process starts.

## Design

The scheduler will calculate the next occurrence from the current local clock,
sleep until that occurrence, and then re-read settings before starting the
scheduled batch. After each wake-up it will calculate a fresh occurrence, so a
schedule changed through the UI takes effect without waiting for the old
schedule. The existing serial batch behavior and cancellation handling remain
unchanged.

## Testing

Add unit coverage for a weekly schedule whose process check phase is not aligned
to the minute boundary. The test will assert that the next occurrence is the
scheduled Friday at 05:00, exercising the same cron calculation used by the
scheduler. Existing cron parsing and full-suite tests must continue to pass.

## Scope

This change addresses missed cron occurrences caused by fixed 30-second polling.
It does not add catch-up behavior for occurrences missed while the service was
stopped, and it does not change the configured timezone semantics.
