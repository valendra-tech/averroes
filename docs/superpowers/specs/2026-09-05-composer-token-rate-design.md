# Composer token-rate indicator

## Goal

Show the response generation speed in the composer as tokens per second while
an agent is streaming, and keep the last measured value visible after the
response finishes until the next response starts.

## Design

The metric belongs to the in-memory shell session, not to persisted
conversation data. Each request resets the current measurement. The first
non-empty text or reasoning delta starts the generation clock; subsequent
deltas update an estimated output-token count and the current rate. This
avoids counting provider connection latency as generation speed.

The live count uses a documented approximation because the provider stream
does not expose tokenizer-level events. When the final provider response
contains `output_tokens`, that authoritative value replaces the estimate for
the final displayed rate. If usage is unavailable, the final estimate is
retained.

The composer footer displays the current or last rate in a compact form such
as `18 tok/s`, with a tooltip distinguishing an estimate from an exact
provider-reported value. The value remains visible after completion, is
cleared when a new request begins, and is not written to the database.

## Data flow

1. Starting a request clears the prior rate and initializes ephemeral counters.
2. `TextDelta` and `ReasoningDelta` events feed the response-rate tracker.
3. GPUI notifications repaint the composer with the current rate.
4. Request completion finalizes the rate using provider usage when available.
5. Errors and cancellation retain the last valid rate for that request.

## Alternatives considered

### Persist the metric with the conversation

Rejected: a speed measurement is execution metadata and becomes misleading
after reopening a conversation.

### Derive the metric only from rendered message text

Rejected: this misses reasoning deltas and couples measurement to presentation
state. The stream-event path is the authoritative source for live updates.

### Add tokenizer dependencies for every provider

Rejected for this iteration: providers use different tokenizers and may not
expose the same accounting rules. The approximation is stable across
providers, while final provider usage is preferred when available.

## Error handling and boundaries

The indicator is informational only. It must never affect request execution,
retry behavior, budgeting, persistence, or cancellation. A response with no
text or no usable elapsed time leaves the indicator absent or keeps the last
valid value. The tracker must tolerate very small elapsed durations without
division-by-zero or implausibly large display values.

## Testing

- Unit-test token estimation, elapsed-time handling, rate formatting, and
  reset/finalization behavior.
- Verify that text and reasoning deltas update the same measurement.
- Verify that provider-reported output usage replaces the estimate.
- Run core and GPUI test suites plus workspace checks and formatting checks.
